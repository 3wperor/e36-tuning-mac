//! Siemens MS41 DME (M52 / S52 engines, E36 and early E39).
//!
//! Only read-only diagnostics plus fault clearing live here so far. Nothing in
//! this module touches the ECU's flash.

use crate::session::{Session, SessionError};
use crate::transport::Transport;

/// DS2 address of the engine control unit (DME).
pub const DME_ADDRESS: u8 = 0x12;

pub mod cmd {
    pub const IDENT: u8 = 0x00;
    pub const READ_FAULTS: u8 = 0x04;
    pub const CLEAR_FAULTS: u8 = 0x05;
}

/// The ECU's identification block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    /// BMW part number of the DME, e.g. `1429861`, if the reply starts with
    /// seven ASCII digits as DS2 ident blocks normally do.
    pub part_number: Option<String>,
    pub raw: Vec<u8>,
}

impl Ident {
    pub fn parse(data: &[u8]) -> Self {
        let part_number = data
            .get(..7)
            .filter(|p| p.iter().all(u8::is_ascii_digit))
            .map(|p| String::from_utf8_lossy(p).into_owned());
        Self {
            part_number,
            raw: data.to_vec(),
        }
    }

    /// Part number in BMW's printed style: `1 429 861`.
    pub fn part_number_pretty(&self) -> Option<String> {
        self.part_number
            .as_ref()
            .map(|p| format!("{} {} {}", &p[..1], &p[1..4], &p[4..]))
    }

    /// The raw block as text, with non-printable bytes shown as `.`.
    pub fn ascii(&self) -> String {
        self.raw
            .iter()
            .map(|&b| {
                if b.is_ascii_graphic() || b == b' ' {
                    b as char
                } else {
                    '.'
                }
            })
            .collect()
    }
}

/// Raw fault memory. Decoding the individual codes is MS41-specific and will be
/// added once we have captured real replies from a car to test against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultMemory {
    pub raw: Vec<u8>,
}

/// Request for the "general values" live data block (group 0x0B, subgroup 0x03).
pub const LIVE_REQUEST: [u8; 2] = [0x0B, 0x03];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    U8,
    U16Be,
}

/// One value inside the live data block: `value = raw * scale + add`.
#[derive(Debug, Clone, Copy)]
pub struct Channel {
    pub name: &'static str,
    pub unit: &'static str,
    /// Byte offset from the first byte after the status byte.
    pub offset: usize,
    pub width: Width,
    pub scale: f64,
    pub add: f64,
    /// Expected range, for gauges.
    pub min: f64,
    pub max: f64,
}

impl Channel {
    pub fn decode(&self, data: &[u8]) -> Option<f64> {
        let raw = match self.width {
            Width::U8 => *data.get(self.offset)? as f64,
            Width::U16Be => {
                let b = data.get(self.offset..self.offset + 2)?;
                u16::from_be_bytes([b[0], b[1]]) as f64
            }
        };
        Some(raw * self.scale + self.add)
    }

    /// The inverse of [`decode`](Self::decode); used by the simulator.
    pub fn encode(&self, value: f64, data: &mut [u8]) {
        let raw = ((value - self.add) / self.scale).round();
        match self.width {
            Width::U8 => {
                if let Some(b) = data.get_mut(self.offset) {
                    *b = raw.clamp(0.0, u8::MAX as f64) as u8;
                }
            }
            Width::U16Be => {
                if let Some(b) = data.get_mut(self.offset..self.offset + 2) {
                    b.copy_from_slice(&(raw.clamp(0.0, u16::MAX as f64) as u16).to_be_bytes());
                }
            }
        }
    }
}

/// UNVERIFIED. These offsets and formulas come from community forum code for
/// MS41.x and have not been checked against a real car; MS41 variants differ
/// and sources disagree. Confirm each one with the raw byte view before
/// trusting it.
pub const LIVE_CHANNELS: &[Channel] = &[
    Channel {
        name: "Engine speed",
        unit: "rpm",
        offset: 0,
        width: Width::U16Be,
        scale: 1.0,
        add: 0.0,
        min: 0.0,
        max: 7000.0,
    },
    Channel {
        name: "Intake air temp",
        unit: "°C",
        offset: 6,
        width: Width::U8,
        scale: -0.458,
        add: 103.4,
        min: -10.0,
        max: 80.0,
    },
    Channel {
        name: "Coolant temp",
        unit: "°C",
        offset: 7,
        width: Width::U8,
        scale: -0.45,
        add: 108.0,
        min: -10.0,
        max: 130.0,
    },
    Channel {
        name: "Ignition timing",
        unit: "°",
        offset: 8,
        width: Width::U8,
        scale: 0.373,
        add: -23.6,
        min: -20.0,
        max: 60.0,
    },
];

/// Reads the raw live data block (everything after the status byte).
pub fn read_live<T: Transport>(session: &mut Session<T>) -> Result<Vec<u8>, SessionError> {
    session.request_ok(DME_ADDRESS, &LIVE_REQUEST)
}

pub fn ident<T: Transport>(session: &mut Session<T>) -> Result<Ident, SessionError> {
    session
        .request_ok(DME_ADDRESS, &[cmd::IDENT])
        .map(|d| Ident::parse(&d))
}

pub fn read_faults<T: Transport>(session: &mut Session<T>) -> Result<FaultMemory, SessionError> {
    session
        .request_ok(DME_ADDRESS, &[cmd::READ_FAULTS])
        .map(|raw| FaultMemory { raw })
}

pub fn clear_faults<T: Transport>(session: &mut Session<T>) -> Result<(), SessionError> {
    session
        .request_ok(DME_ADDRESS, &[cmd::CLEAR_FAULTS])
        .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Frame;
    use crate::transport::MockTransport;

    fn reply(payload: &[u8]) -> Vec<u8> {
        Frame::new(DME_ADDRESS, payload).unwrap().encode()
    }

    #[test]
    fn parses_ident_part_number() {
        let id = Ident::parse(b"1429861\x00\x01AB");
        assert_eq!(id.part_number.as_deref(), Some("1429861"));
        assert_eq!(id.part_number_pretty().as_deref(), Some("1 429 861"));
        assert_eq!(id.ascii(), "1429861..AB");
    }

    #[test]
    fn ident_without_digits_keeps_raw() {
        let id = Ident::parse(&[0x01, 0x02]);
        assert_eq!(id.part_number, None);
        assert_eq!(id.raw, vec![0x01, 0x02]);
    }

    #[test]
    fn ident_over_mock_ecu() {
        let mut payload = vec![0xA0];
        payload.extend_from_slice(b"1429861");
        let mock = MockTransport::new(true).expect([0x12, 0x04, 0x00, 0x16], reply(&payload));
        let id = ident(&mut Session::new(mock)).unwrap();
        assert_eq!(id.part_number.as_deref(), Some("1429861"));
    }

    #[test]
    fn channels_roundtrip_through_encode() {
        let mut data = vec![0u8; 32];
        let values = [3150.0, 25.0, 90.0, 18.0];
        for (ch, v) in LIVE_CHANNELS.iter().zip(values) {
            ch.encode(v, &mut data);
        }
        for (ch, v) in LIVE_CHANNELS.iter().zip(values) {
            let got = ch.decode(&data).unwrap();
            assert!(
                (got - v).abs() <= ch.scale.abs(),
                "{}: {got} vs {v}",
                ch.name
            );
        }
    }

    #[test]
    fn channel_decode_on_short_block_is_none() {
        assert_eq!(LIVE_CHANNELS[0].decode(&[0x12]), None);
    }

    #[test]
    fn read_live_over_mock_ecu() {
        let mock =
            MockTransport::new(true).expect([0x12, 0x05, 0x0B, 0x03, 0x1F], reply(&[0xA0, 1, 2]));
        assert_eq!(read_live(&mut Session::new(mock)).unwrap(), vec![1, 2]);
    }

    #[test]
    fn read_and_clear_faults_over_mock_ecu() {
        let mock = MockTransport::new(true)
            .expect([0x12, 0x04, 0x04, 0x12], reply(&[0xA0, 0x01, 0x23]))
            .expect([0x12, 0x04, 0x05, 0x13], reply(&[0xA0]));
        let mut s = Session::new(mock);
        assert_eq!(read_faults(&mut s).unwrap().raw, vec![0x01, 0x23]);
        clear_faults(&mut s).unwrap();
        assert!(s.transport().is_finished());
    }
}
