use serde::{Deserialize, Serialize};
#[cfg(test)]
use sha2::{Digest, Sha256};

pub const MAX_HEADER_BYTES: usize = 4096;
pub const CHUNK_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Operation {
    Download,
    Upload,
    Duplex,
    HalfClose,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Request {
    pub token: String,
    pub operation: Operation,
    pub max_bytes: u64,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub rate_bytes_per_sec: u64,
    #[serde(default)]
    pub receive_rate_bytes_per_sec: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

pub fn payload_pattern(offset: u64) -> u8 {
    ((offset.wrapping_mul(31).wrapping_add(7)) % 251) as u8
}

pub fn fill_payload(offset: u64, output: &mut [u8]) {
    for (index, byte) in output.iter_mut().enumerate() {
        *byte = payload_pattern(offset + index as u64);
    }
}

pub fn payload_is_valid(offset: u64, input: &[u8]) -> bool {
    input
        .iter()
        .enumerate()
        .all(|(index, byte)| *byte == payload_pattern(offset + index as u64))
}

#[cfg(test)]
pub fn sha256_hex(input: &[u8]) -> String {
    format!("{:x}", Sha256::digest(input))
}

#[cfg(test)]
mod tests {
    use super::{fill_payload, payload_is_valid, sha256_hex};

    #[test]
    fn deterministic_data_verifies_across_chunk_boundaries() {
        let mut payload = vec![0; 257];
        fill_payload(193, &mut payload);
        assert!(payload_is_valid(193, &payload));
        payload[33] ^= 0xff;
        assert!(!payload_is_valid(193, &payload));
    }

    #[test]
    fn sha256_is_stable() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
