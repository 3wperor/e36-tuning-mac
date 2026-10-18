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
