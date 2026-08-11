use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::Duration,
};

use anyhow::{Context, Result, bail};

pub const PACKET_SIZE: usize = 64;
pub const FIRST_PAYLOAD_OFFSET: usize = 16;
pub const PAYLOAD_OFFSET: usize = 8;
pub const MAX_FIRMWARE_SIZE: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwarePacket {
    pub id: u32,
    pub expected_ack_id: u32,
    pub checksum: u32,
    pub bytes: [u8; PACKET_SIZE],
}

fn write_word(packet: &mut [u8], offset: usize, word: u32) {
    packet[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
}

fn read_word(packet: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        packet[offset..offset + 4]
            .try_into()
            .expect("four-byte word"),
    )
}

pub fn packet_checksum(packet: &[u8]) -> u32 {
    packet.iter().map(|byte| *byte as u32).sum()
}

pub fn build_packets(firmware: &[u8]) -> Result<Vec<FirmwarePacket>> {
    if firmware.is_empty() {
        bail!("firmware image is empty");
    }
    if firmware.len() > MAX_FIRMWARE_SIZE {
        bail!(
            "firmware image is {} bytes; maximum is {MAX_FIRMWARE_SIZE}",
            firmware.len()
        );
    }
    let mut packets = Vec::new();
    let mut offset = 0;
    let mut id = 1_u32;
    while offset < firmware.len() {
        let mut bytes = [0_u8; PACKET_SIZE];
        let payload_offset = if id == 1 {
            write_word(&mut bytes, 0, 0xa0);
            write_word(&mut bytes, 12, firmware.len() as u32);
            FIRST_PAYLOAD_OFFSET
        } else {
            PAYLOAD_OFFSET
        };
        write_word(&mut bytes, 4, id);
        let count = (PACKET_SIZE - payload_offset).min(firmware.len() - offset);
        bytes[payload_offset..payload_offset + count]
            .copy_from_slice(&firmware[offset..offset + count]);
        offset += count;
        packets.push(FirmwarePacket {
            id,
            expected_ack_id: id + 1,
            checksum: packet_checksum(&bytes),
            bytes,
        });
        // The vendor implementation uses odd transmit IDs and expects the following even ID in each ACK.
        id = id.checked_add(2).context("firmware packet ID overflow")?;
    }
    Ok(packets)
}

pub fn validate_ack(packet: &FirmwarePacket, acknowledgement: &[u8]) -> Result<()> {
    if acknowledgement.len() != PACKET_SIZE {
        bail!(
            "firmware ACK is {} bytes; expected {PACKET_SIZE}",
            acknowledgement.len()
        );
    }
    let checksum = read_word(acknowledgement, 0);
    let id = read_word(acknowledgement, 4);
    if checksum != packet.checksum {
        bail!(
            "packet {} checksum mismatch: sent 0x{:08x}, received 0x{checksum:08x}",
            packet.id,
            packet.checksum
        );
    }
    if id != packet.expected_ack_id {
        bail!(
            "packet {} ACK ID mismatch: expected {}, received {id}",
            packet.id,
            packet.expected_ack_id
        );
    }
    Ok(())
}

pub fn load_firmware(path: &Path) -> Result<Vec<u8>> {
    let metadata =
        fs::metadata(path).with_context(|| format!("inspect firmware {}", path.display()))?;
    if metadata.len() == 0 || metadata.len() > MAX_FIRMWARE_SIZE as u64 {
        bail!(
            "firmware {} has unsafe size {} bytes",
            path.display(),
            metadata.len()
        );
    }
    fs::read(path).with_context(|| format!("read firmware {}", path.display()))
}

pub fn flash_serial(path: &Path, firmware: &[u8], retries: u8) -> Result<()> {
    let packets = build_packets(firmware)?;
    let mut serial = serialport::new(path.to_string_lossy(), 115_200)
        .timeout(Duration::from_secs(3))
        .open()
        .with_context(|| format!("open firmware UART {} at 115200 baud", path.display()))?;
    for packet in packets {
        let mut attempt = 0;
        loop {
            attempt += 1;
            serial
                .write_all(&packet.bytes)
                .with_context(|| format!("write firmware packet {}", packet.id))?;
            let mut acknowledgement = [0_u8; PACKET_SIZE];
            match serial
                .read_exact(&mut acknowledgement)
                .context("read 64-byte firmware ACK")
                .and_then(|()| validate_ack(&packet, &acknowledgement))
            {
                Ok(()) => break,
                Err(error) if attempt <= retries => log::warn!(
                    "firmware packet {} attempt {attempt} failed: {error:#}",
                    packet.id
                ),
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "firmware packet {} failed after {attempt} attempt(s)",
                            packet.id
                        )
                    });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_packet_has_exact_legacy_header_and_payload() {
        let firmware = (0_u8..100).collect::<Vec<_>>();
        let packets = build_packets(&firmware).unwrap();
        assert_eq!(packets[0].id, 1);
        assert_eq!(read_word(&packets[0].bytes, 0), 0xa0);
        assert_eq!(read_word(&packets[0].bytes, 4), 1);
        assert_eq!(read_word(&packets[0].bytes, 12), 100);
        assert_eq!(&packets[0].bytes[16..], &firmware[..48]);
        assert_eq!(packets[1].id, 3);
        assert_eq!(read_word(&packets[1].bytes, 4), 3);
        assert_eq!(&packets[1].bytes[8..60], &firmware[48..]);
    }

    #[test]
    fn ack_validation_fails_closed() {
        let packet = build_packets(&[1, 2, 3]).unwrap().remove(0);
        let mut ack = [0_u8; PACKET_SIZE];
        write_word(&mut ack, 0, packet.checksum);
        write_word(&mut ack, 4, packet.expected_ack_id);
        validate_ack(&packet, &ack).unwrap();
        ack[0] ^= 1;
        assert!(validate_ack(&packet, &ack).is_err());
    }

    #[test]
    fn rejects_empty_and_excessive_images() {
        assert!(build_packets(&[]).is_err());
        assert!(build_packets(&vec![0; MAX_FIRMWARE_SIZE + 1]).is_err());
    }
}
