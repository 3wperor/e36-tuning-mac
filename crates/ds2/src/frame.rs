//! Single DS2 frames: `[address] [length] [payload ...] [checksum]`.

use thiserror::Error;

/// Address byte + length byte + checksum byte.
pub const FRAME_OVERHEAD: usize = 3;
/// The length byte is a `u8`, so a frame can never exceed 255 bytes.
pub const MAX_FRAME_LEN: usize = u8::MAX as usize;
pub const MAX_PAYLOAD_LEN: usize = MAX_FRAME_LEN - FRAME_OVERHEAD;

/// XOR of all bytes — the DS2 checksum.
pub fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |acc, b| acc ^ b)
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FrameError {
    #[error("frame too short: {0} bytes (minimum {FRAME_OVERHEAD})")]
    TooShort(usize),
    #[error("payload too long: {0} bytes (maximum {MAX_PAYLOAD_LEN})")]
    PayloadTooLong(usize),
    #[error("length byte says {declared} bytes but frame has {actual}")]
    LengthMismatch { declared: usize, actual: usize },
    #[error("bad checksum: expected {expected:#04X}, got {actual:#04X}")]
    BadChecksum { expected: u8, actual: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub address: u8,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(address: u8, payload: impl Into<Vec<u8>>) -> Result<Self, FrameError> {
        let payload = payload.into();
        if payload.len() > MAX_PAYLOAD_LEN {
            return Err(FrameError::PayloadTooLong(payload.len()));
        }
        Ok(Self { address, payload })
    }

    /// Total length on the wire, as written into the length byte.
    pub fn wire_len(&self) -> usize {
        self.payload.len() + FRAME_OVERHEAD
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.wire_len());
        out.push(self.address);
        out.push(self.wire_len() as u8);
        out.extend_from_slice(&self.payload);
        out.push(checksum(&out));
        out
    }

    /// Decodes exactly one complete frame; `bytes` must contain nothing else.
    pub fn decode(bytes: &[u8]) -> Result<Self, FrameError> {
        if bytes.len() < FRAME_OVERHEAD {
            return Err(FrameError::TooShort(bytes.len()));
        }
        let declared = bytes[1] as usize;
        if declared != bytes.len() {
            return Err(FrameError::LengthMismatch {
                declared,
                actual: bytes.len(),
            });
        }
        let (body, last) = bytes.split_at(bytes.len() - 1);
        let expected = checksum(body);
        if expected != last[0] {
            return Err(FrameError::BadChecksum {
                expected,
                actual: last[0],
            });
        }
        Ok(Self {
            address: bytes[0],
            payload: body[2..].to_vec(),
        })
    }
}

/// First payload byte of every ECU reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Busy,
    Rejected,
    ParameterError,
    FunctionError,
    NumberError,
    NotAcknowledged,
    Unknown(u8),
}

impl Status {
    pub fn is_ok(self) -> bool {
        self == Status::Ok
    }
}

impl From<u8> for Status {
    fn from(b: u8) -> Self {
        match b {
            0xA0 => Status::Ok,
            0xA1 => Status::Busy,
            0xA2 => Status::Rejected,
            0xB0 => Status::ParameterError,
            0xB1 => Status::FunctionError,
            0xB2 => Status::NumberError,
            0xFF => Status::NotAcknowledged,
            other => Status::Unknown(other),
        }
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Ok => write!(f, "OK (A0)"),
            Status::Busy => write!(f, "busy (A1)"),
            Status::Rejected => write!(f, "rejected (A2)"),
            Status::ParameterError => write!(f, "parameter error (B0)"),
            Status::FunctionError => write!(f, "function error (B1)"),
            Status::NumberError => write!(f, "number error (B2)"),
            Status::NotAcknowledged => write!(f, "not acknowledged (FF)"),
            Status::Unknown(b) => write!(f, "unknown status ({b:02X})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_matches_known_ms41_requests() {
        assert_eq!(checksum(&[0x12, 0x04, 0x00]), 0x16); // ident
        assert_eq!(checksum(&[0x12, 0x04, 0x04]), 0x12); // read faults
        assert_eq!(checksum(&[0x12, 0x04, 0x05]), 0x13); // clear faults
    }

    #[test]
    fn encodes_ident_request() {
        let f = Frame::new(0x12, vec![0x00]).unwrap();
        assert_eq!(f.encode(), vec![0x12, 0x04, 0x00, 0x16]);
    }

    #[test]
    fn roundtrips() {
        let f = Frame::new(0x12, vec![0xA0, 0x31, 0x34, 0x32, 0x39]).unwrap();
        assert_eq!(Frame::decode(&f.encode()).unwrap(), f);
    }

    #[test]
    fn rejects_bad_checksum() {
        assert_eq!(
            Frame::decode(&[0x12, 0x04, 0x00, 0x17]),
            Err(FrameError::BadChecksum {
                expected: 0x16,
                actual: 0x17
            })
        );
    }

    #[test]
    fn rejects_length_mismatch_and_short_frames() {
        assert!(matches!(
            Frame::decode(&[0x12, 0x05, 0x00, 0x17]),
            Err(FrameError::LengthMismatch {
                declared: 5,
                actual: 4
            })
        ));
        assert_eq!(Frame::decode(&[0x12, 0x02]), Err(FrameError::TooShort(2)));
    }

    #[test]
    fn rejects_oversized_payload() {
        assert_eq!(
            Frame::new(0x12, vec![0; 253]),
            Err(FrameError::PayloadTooLong(253))
        );
        assert!(Frame::new(0x12, vec![0; 252]).is_ok());
    }

    #[test]
    fn maps_status_bytes() {
        assert_eq!(Status::from(0xA0), Status::Ok);
        assert_eq!(Status::from(0xA2), Status::Rejected);
        assert_eq!(Status::from(0x42), Status::Unknown(0x42));
    }
}
