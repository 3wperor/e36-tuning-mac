//! Siemens MS41 DME (M52 / S52 engines, E36 and early E39).
//!
//! Only read-only diagnostics plus fault clearing live here so far. Nothing in
//! this module touches the ECU's flash.

pub mod fault_codes;

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

/// Requests tried in order to read the fault memory. A forum bench capture of
/// an MS41 uses `04 01`; plain `04` is the generic DS2 form.
pub const READ_FAULTS_REQUESTS: [&[u8]; 2] = [&[cmd::READ_FAULTS, 0x01], &[cmd::READ_FAULTS]];

/// Size of one stored fault in the reply, from the same forum capture.
/// UNVERIFIED for your ECU.
pub const FAULT_RECORD_LEN: usize = 8;

/// The fault memory exactly as the DME sent it (after the status byte).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultMemory {
    pub raw: Vec<u8>,
}

/// One stored fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    /// BMW decimal fault code.
    pub code: u8,
    /// The whole record, code included. The other bytes (fault type, how
    /// often it happened, conditions at the time) aren't decoded yet.
    pub record: Vec<u8>,
}

impl Fault {
    pub fn description(&self) -> Option<&'static str> {
        fault_codes::describe(self.code)
    }
}

impl FaultMemory {
    /// Best-effort decode, assuming the layout seen in a forum capture: a count
    /// byte, then one fixed-size record per fault starting with its code.
    ///
    /// Returns `None` when the reply doesn't fit that layout, so callers can
    /// fall back to the raw bytes instead of showing made-up codes.
    pub fn decode(&self) -> Option<Vec<Fault>> {
        let (&count, records) = self.raw.split_first()?;
        let count = count as usize;
        if count == 0 {
            return Some(Vec::new());
        }
        let size =
            if records.len() % FAULT_RECORD_LEN == 0 && count * FAULT_RECORD_LEN <= records.len() {
                FAULT_RECORD_LEN
            } else if !records.is_empty() && records.len() % count == 0 {
                records.len() / count
            } else {
                return None;
            };
        Some(
            records
                .chunks(size)
                .take(count)
                .map(|r| Fault {
                    code: r[0],
                    record: r.to_vec(),
                })
                .collect(),
        )
    }
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

/// The engine speed channel, used to tag events such as VANOS switching.
pub const ENGINE_SPEED: Channel = LIVE_CHANNELS[0];

/// Where an on/off flag (such as VANOS) sits in the live data block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flag {
    pub offset: usize,
    /// Bit within the byte (0 = least significant). `None` means the whole
    /// byte, read as on when it isn't zero.
    pub bit: Option<u8>,
    /// Swap on and off, for flags the ECU stores the other way round.
    pub invert: bool,
}

impl Flag {
    pub fn decode(&self, data: &[u8]) -> Option<bool> {
        let byte = *data.get(self.offset)?;
        let on = match self.bit {
            Some(n) => (byte >> n) & 1 == 1,
            None => byte != 0,
        };
        Some(on != self.invert)
    }
}

/// Request for the "global knock retard" group (0B 93). From a forum report
/// that MS41s answer it in BMW's INPA/TESTO tools. The reply's layout and units
/// are UNVERIFIED, so it's shown and recorded as raw bytes for now.
pub const KNOCK_REQUEST: [u8; 2] = [0x0B, 0x93];

/// Reads the knock retard group (everything after the status byte).
pub fn read_knock<T: Transport>(session: &mut Session<T>) -> Result<Vec<u8>, SessionError> {
    session.request_ok(DME_ADDRESS, &KNOCK_REQUEST)
}

/// The cylinder (1-6) a misfire fault code (238-243) refers to.
pub fn misfire_cylinder(code: u8) -> Option<u8> {
    (238..=243).contains(&code).then(|| code - 237)
}

/// The knock sensor (1: cylinders 1-3, 2: cylinders 4-6) a fault code refers to.
pub fn knock_sensor(code: u8) -> Option<u8> {
    match code {
        57 => Some(1),
        59 => Some(2),
        _ => None,
    }
}

/// Reads the raw live data block (everything after the status byte).
pub fn read_live<T: Transport>(session: &mut Session<T>) -> Result<Vec<u8>, SessionError> {
    session.request_ok(DME_ADDRESS, &LIVE_REQUEST)
}

pub fn ident<T: Transport>(session: &mut Session<T>) -> Result<Ident, SessionError> {
    session
        .request_ok(DME_ADDRESS, &[cmd::IDENT])
        .map(|d| Ident::parse(&d))
}

/// Reads the fault memory, trying each of [`READ_FAULTS_REQUESTS`] until the
/// DME accepts one.
pub fn read_faults<T: Transport>(session: &mut Session<T>) -> Result<FaultMemory, SessionError> {
    let mut rejected = None;
    for request in READ_FAULTS_REQUESTS {
        match session.request_ok(DME_ADDRESS, request) {
            Ok(raw) => return Ok(FaultMemory { raw }),
            Err(SessionError::Negative(status)) => rejected = Some(status),
            Err(e) => return Err(e),
        }
    }
    Err(SessionError::Negative(
        rejected.expect("at least one request"),
    ))
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
    fn flag_reads_bits_bytes_and_inversion() {
        let data = [0x00, 0b0000_1000];
        let bit3 = Flag {
            offset: 1,
            bit: Some(3),
            invert: false,
        };
        assert_eq!(bit3.decode(&data), Some(true));
        assert_eq!(
            Flag {
                bit: Some(2),
                ..bit3
            }
            .decode(&data),
            Some(false)
        );
        assert_eq!(
            Flag {
                invert: true,
                ..bit3
            }
            .decode(&data),
            Some(false)
        );
        assert_eq!(
            Flag {
                offset: 0,
                bit: None,
                ..bit3
            }
            .decode(&data),
            Some(false)
        );
        assert_eq!(Flag { bit: None, ..bit3 }.decode(&data), Some(true));
        assert_eq!(Flag { offset: 5, ..bit3 }.decode(&data), None);
    }

    #[test]
    fn recognises_misfire_and_knock_sensor_codes() {
        assert_eq!(misfire_cylinder(238), Some(1));
        assert_eq!(misfire_cylinder(243), Some(6));
        assert_eq!(misfire_cylinder(237), None);
        assert_eq!(misfire_cylinder(244), None);
        assert_eq!(knock_sensor(57), Some(1));
        assert_eq!(knock_sensor(59), Some(2));
        assert_eq!(knock_sensor(58), None);
    }

    #[test]
    fn read_knock_over_mock_ecu() {
        let mock =
            MockTransport::new(true).expect([0x12, 0x05, 0x0B, 0x93, 0x8F], reply(&[0xA0, 7, 0]));
        assert_eq!(read_knock(&mut Session::new(mock)).unwrap(), vec![7, 0]);
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
            .expect([0x12, 0x05, 0x04, 0x01, 0x12], reply(&[0xA0, 0x01, 0x23]))
            .expect([0x12, 0x04, 0x05, 0x13], reply(&[0xA0]));
        let mut s = Session::new(mock);
        assert_eq!(read_faults(&mut s).unwrap().raw, vec![0x01, 0x23]);
        clear_faults(&mut s).unwrap();
        assert!(s.transport().is_finished());
    }

    #[test]
    fn read_faults_falls_back_to_plain_request() {
        let mock = MockTransport::new(true)
            .expect([0x12, 0x05, 0x04, 0x01, 0x12], reply(&[0xB0]))
            .expect([0x12, 0x04, 0x04, 0x12], reply(&[0xA0, 0x00]));
        let mut s = Session::new(mock);
        assert_eq!(read_faults(&mut s).unwrap().raw, vec![0x00]);
        assert!(s.transport().is_finished());
    }

    #[test]
    fn read_faults_reports_rejection_when_both_fail() {
        let mock = MockTransport::new(true)
            .expect([0x12, 0x05, 0x04, 0x01, 0x12], reply(&[0xB0]))
            .expect([0x12, 0x04, 0x04, 0x12], reply(&[0xA2]));
        let err = read_faults(&mut Session::new(mock)).unwrap_err();
        assert!(matches!(
            err,
            SessionError::Negative(crate::Status::Rejected)
        ));
    }

    fn record(code: u8) -> Vec<u8> {
        let mut r = vec![0u8; FAULT_RECORD_LEN];
        r[0] = code;
        r[1] = 0x11;
        r
    }

    #[test]
    fn decodes_count_and_records() {
        let raw = [vec![2], record(10), record(243)].concat();
        let faults = FaultMemory { raw }.decode().unwrap();
        assert_eq!(faults.len(), 2);
        assert_eq!(faults[0].code, 10);
        assert_eq!(faults[0].record, record(10));
        assert_eq!(faults[1].description(), Some("Misfire, cylinder 6"));
    }

    #[test]
    fn decodes_fixed_slots_with_fewer_faults() {
        // Ten slots sent, only the first one in use.
        let mut raw = vec![1];
        raw.extend(record(57));
        raw.extend(vec![0u8; FAULT_RECORD_LEN * 9]);
        let faults = FaultMemory { raw }.decode().unwrap();
        assert_eq!(faults.len(), 1);
        assert_eq!(faults[0].code, 57);
    }

    #[test]
    fn decodes_other_record_sizes_when_consistent() {
        let raw = vec![2, 10, 0xAA, 0xBB, 14, 0xCC, 0xDD];
        let faults = FaultMemory { raw }.decode().unwrap();
        assert_eq!(faults.iter().map(|f| f.code).collect::<Vec<_>>(), [10, 14]);
        assert_eq!(faults[1].record, vec![14, 0xCC, 0xDD]);
    }

    #[test]
    fn empty_memory_decodes_to_no_faults() {
        assert_eq!(FaultMemory { raw: vec![0] }.decode(), Some(vec![]));
        assert_eq!(
            FaultMemory {
                raw: vec![0; 1 + FAULT_RECORD_LEN * 10]
            }
            .decode(),
            Some(vec![])
        );
    }

    #[test]
    fn unrecognised_layout_is_none() {
        assert_eq!(FaultMemory { raw: vec![] }.decode(), None);
        assert_eq!(
            FaultMemory {
                raw: vec![3, 1, 2, 3, 4]
            }
            .decode(),
            None
        );
        assert_eq!(FaultMemory { raw: vec![5] }.decode(), None);
    }
}
