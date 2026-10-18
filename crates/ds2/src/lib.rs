//! BMW DS2 diagnostic protocol over a K-line.
//!
//! DS2 is the protocol BMW ECUs of the E36 era speak on the diagnostic bus
//! (9600 baud, 8 data bits, even parity, 1 stop bit). Every message is:
//!
//! ```text
//! [address] [length] [payload ...] [checksum]
//! ```
//!
//! where `length` counts every byte of the frame (including itself and the
//! checksum) and `checksum` is the XOR of all preceding bytes. ECU replies use
//! the same layout with a status byte (`0xA0` = OK) as the first payload byte.
//!
//! Layout of this crate:
//! - [`frame`]: encoding/decoding of single frames (pure, no I/O)
//! - [`transport`]: the byte-pipe abstraction plus a scripted mock for tests
//! - [`session`]: request/response handling, K-line echo, timeouts
//! - [`ms41`]: commands for the Siemens MS41 DME (M52/S52 engines)
//! - `serial` (feature `serial`): the real USB cable

pub mod frame;
pub mod ms41;
#[cfg(feature = "serial")]
pub mod serial;
pub mod session;
pub mod transport;

pub use frame::{checksum, Frame, FrameError, Status};
pub use session::{EchoMode, Response, Session, SessionError, Timeouts, Trace};
pub use transport::{MockTransport, Transport};

/// Formats bytes as space-separated upper-case hex, e.g. `12 04 00 16`.
pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn hex_formats_bytes() {
        assert_eq!(super::hex(&[0x12, 0x04, 0x00, 0x16]), "12 04 00 16");
        assert_eq!(super::hex(&[]), "");
    }
}
