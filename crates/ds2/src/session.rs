//! Request/response exchanges over a K-line.
//!
//! The K-line is a single wire, so most cables hand back every byte we send
//! (the "echo") before the ECU's reply arrives. [`EchoMode::Auto`] copes with
//! cables that do and don't echo.

use std::io;
use std::time::Duration;

use thiserror::Error;

use crate::frame::{Frame, FrameError, Status, FRAME_OVERHEAD};
use crate::transport::Transport;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EchoMode {
    /// Discard the echo if it shows up, otherwise treat the bytes as the reply.
    Auto,
    /// The cable always echoes; anything else is an error.
    Expect,
    /// The cable never echoes.
    Off,
}

#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    /// How long to wait for the first byte after sending a request.
    pub first_byte: Duration,
    /// How long to wait between bytes once a frame has started.
    pub inter_byte: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        // One byte at 9600 8E1 takes ~1.1 ms; the slack covers USB adapter
        // latency (FTDI defaults to 16 ms) and slow ECU responses.
        Self {
            first_byte: Duration::from_millis(1000),
            inter_byte: Duration::from_millis(100),
        }
    }
}

/// What a trace callback is shown.
#[derive(Debug, Clone, Copy)]
pub enum Trace<'a> {
    Tx(&'a [u8]),
    Echo(&'a [u8]),
    Rx(&'a [u8]),
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("serial I/O error: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("no response from ECU (check ignition is on, cable switch, and port)")]
    NoResponse,
    #[error("response cut off after {received} bytes")]
    Incomplete { received: usize },
    #[error("echo mismatch: sent {sent}, read back {received}")]
    EchoMismatch { sent: String, received: String },
    #[error("reply came from address {actual:#04X}, expected {expected:#04X}")]
    WrongAddress { expected: u8, actual: u8 },
    #[error("reply has no status byte")]
    EmptyResponse,
    #[error("ECU answered {0}")]
    Negative(Status),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: Status,
    /// Payload after the status byte.
    pub data: Vec<u8>,
}

type TraceFn = Box<dyn FnMut(Trace<'_>)>;

pub struct Session<T: Transport> {
    transport: T,
    echo: EchoMode,
    timeouts: Timeouts,
    trace: Option<TraceFn>,
}

impl<T: Transport> Session<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            echo: EchoMode::Auto,
            timeouts: Timeouts::default(),
            trace: None,
        }
    }

    pub fn with_echo(mut self, echo: EchoMode) -> Self {
        self.echo = echo;
        self
    }

    pub fn with_timeouts(mut self, timeouts: Timeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    pub fn with_trace(mut self, trace: impl FnMut(Trace<'_>) + 'static) -> Self {
        self.trace = Some(Box::new(trace));
        self
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Sends one request and returns the reply, whatever its status.
    pub fn request(&mut self, address: u8, payload: &[u8]) -> Result<Response, SessionError> {
        let tx = Frame::new(address, payload)?.encode();
        self.transport.clear_input()?;
        self.transport.write_all(&tx)?;
        self.emit(Trace::Tx(&tx));

        let mut rx = Vec::new();
        if self.echo != EchoMode::Off {
            // Read while the bytes still match what we sent, so a reply that is
            // shorter than the request is never over-read.
            while rx.len() < tx.len() && tx.starts_with(&rx) {
                let want = rx.len() + 1;
                self.read_into(&mut rx, want)?;
            }
            if rx == tx {
                self.emit(Trace::Echo(&rx));
                rx.clear();
            } else if self.echo == EchoMode::Expect {
                return Err(SessionError::EchoMismatch {
                    sent: crate::hex(&tx),
                    received: crate::hex(&rx),
                });
            }
            // Auto: what we read was the start of the reply, keep it.
        }

        self.read_into(&mut rx, 2)?;
        let declared = rx[1] as usize;
        if declared < FRAME_OVERHEAD {
            self.emit(Trace::Rx(&rx));
            return Err(FrameError::TooShort(declared).into());
        }
        self.read_into(&mut rx, declared)?;
        self.emit(Trace::Rx(&rx));

        let frame = Frame::decode(&rx)?;
        if frame.address != address {
            return Err(SessionError::WrongAddress {
                expected: address,
                actual: frame.address,
            });
        }
        let (&status, data) = frame
            .payload
            .split_first()
            .ok_or(SessionError::EmptyResponse)?;
        Ok(Response {
            status: status.into(),
            data: data.to_vec(),
        })
    }

    /// Like [`request`](Self::request) but treats any non-OK status as an error.
    pub fn request_ok(&mut self, address: u8, payload: &[u8]) -> Result<Vec<u8>, SessionError> {
        let resp = self.request(address, payload)?;
        if !resp.status.is_ok() {
            return Err(SessionError::Negative(resp.status));
        }
        Ok(resp.data)
    }

    /// Reads until `buf` holds `len` bytes. Bytes already in `buf` count.
    fn read_into(&mut self, buf: &mut Vec<u8>, len: usize) -> Result<(), SessionError> {
        while buf.len() < len {
            let timeout = if buf.is_empty() {
                self.timeouts.first_byte
            } else {
                self.timeouts.inter_byte
            };
            match self.transport.read_byte(timeout)? {
                Some(b) => buf.push(b),
                None if buf.is_empty() => return Err(SessionError::NoResponse),
                None => {
                    self.emit(Trace::Rx(buf));
                    return Err(SessionError::Incomplete {
                        received: buf.len(),
                    });
                }
            }
        }
        Ok(())
    }

    fn emit(&mut self, event: Trace<'_>) {
        if let Some(trace) = self.trace.as_mut() {
            trace(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;

    const IDENT_REQ: [u8; 4] = [0x12, 0x04, 0x00, 0x16];

    fn reply(payload: &[u8]) -> Vec<u8> {
        Frame::new(0x12, payload).unwrap().encode()
    }

    #[test]
    fn auto_echo_discards_echo() {
        let mock = MockTransport::new(true).expect(IDENT_REQ, reply(&[0xA0, 0x31, 0x32]));
        let mut s = Session::new(mock);
        let r = s.request(0x12, &[0x00]).unwrap();
        assert_eq!(r.status, Status::Ok);
        assert_eq!(r.data, vec![0x31, 0x32]);
        assert!(s.transport().is_finished());
    }

    #[test]
    fn auto_echo_handles_cable_without_echo() {
        let mock = MockTransport::new(false).expect(IDENT_REQ, reply(&[0xA0, 0x31, 0x32]));
        let r = Session::new(mock).request(0x12, &[0x00]).unwrap();
        assert_eq!(r.data, vec![0x31, 0x32]);
    }

    #[test]
    fn auto_echo_handles_reply_same_length_as_request() {
        // Clear-faults: 4-byte request, 4-byte reply, no echo.
        let mock = MockTransport::new(false).expect([0x12, 0x04, 0x05, 0x13], reply(&[0xA0]));
        let r = Session::new(mock).request(0x12, &[0x05]).unwrap();
        assert_eq!(r.status, Status::Ok);
        assert!(r.data.is_empty());
    }

    #[test]
    fn expect_echo_errors_when_missing() {
        let mock = MockTransport::new(false).expect(IDENT_REQ, reply(&[0xA0, 0x31, 0x32]));
        let err = Session::new(mock)
            .with_echo(EchoMode::Expect)
            .request(0x12, &[0x00])
            .unwrap_err();
        assert!(matches!(err, SessionError::EchoMismatch { .. }));
    }

    #[test]
    fn echo_off_reads_reply_directly() {
        let mock = MockTransport::new(false).expect(IDENT_REQ, reply(&[0xA0, 0x31]));
        let r = Session::new(mock)
            .with_echo(EchoMode::Off)
            .request(0x12, &[0x00])
            .unwrap();
        assert_eq!(r.data, vec![0x31]);
    }

    #[test]
    fn silent_ecu_is_no_response() {
        let mock = MockTransport::new(false);
        let err = Session::new(mock).request(0x12, &[0x00]).unwrap_err();
        assert!(matches!(err, SessionError::NoResponse));
    }

    #[test]
    fn echo_but_no_reply_is_no_response() {
        let mock = MockTransport::new(true);
        let err = Session::new(mock).request(0x12, &[0x00]).unwrap_err();
        assert!(matches!(err, SessionError::NoResponse));
    }

    #[test]
    fn truncated_reply_is_incomplete() {
        let mut r = reply(&[0xA0, 0x31, 0x32, 0x33]);
        r.truncate(4);
        let mock = MockTransport::new(true).expect(IDENT_REQ, r);
        let err = Session::new(mock).request(0x12, &[0x00]).unwrap_err();
        assert!(matches!(err, SessionError::Incomplete { received: 4 }));
    }

    #[test]
    fn corrupted_reply_is_checksum_error() {
        let mut r = reply(&[0xA0, 0x31]);
        *r.last_mut().unwrap() ^= 0xFF;
        let mock = MockTransport::new(true).expect(IDENT_REQ, r);
        let err = Session::new(mock).request(0x12, &[0x00]).unwrap_err();
        assert!(matches!(
            err,
            SessionError::Frame(FrameError::BadChecksum { .. })
        ));
    }

    #[test]
    fn reply_from_other_module_is_rejected() {
        let r = Frame::new(0x32, vec![0xA0]).unwrap().encode();
        let mock = MockTransport::new(true).expect(IDENT_REQ, r);
        let err = Session::new(mock).request(0x12, &[0x00]).unwrap_err();
        assert!(matches!(
            err,
            SessionError::WrongAddress {
                expected: 0x12,
                actual: 0x32
            }
        ));
    }

    #[test]
    fn request_ok_surfaces_negative_status() {
        let mock = MockTransport::new(true).expect(IDENT_REQ, reply(&[0xA2]));
        let err = Session::new(mock).request_ok(0x12, &[0x00]).unwrap_err();
        assert!(matches!(err, SessionError::Negative(Status::Rejected)));
    }

    #[test]
    fn trace_sees_tx_echo_and_rx() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let log = Rc::new(RefCell::new(Vec::new()));
        let sink = log.clone();
        let mock = MockTransport::new(true).expect(IDENT_REQ, reply(&[0xA0]));
        Session::new(mock)
            .with_trace(move |t| {
                sink.borrow_mut().push(match t {
                    Trace::Tx(b) => format!("tx {}", crate::hex(b)),
                    Trace::Echo(b) => format!("echo {}", crate::hex(b)),
                    Trace::Rx(b) => format!("rx {}", crate::hex(b)),
                })
            })
            .request(0x12, &[0x00])
            .unwrap();
        assert_eq!(
            *log.borrow(),
            vec!["tx 12 04 00 16", "echo 12 04 00 16", "rx 12 04 A0 B6"]
        );
    }
}
