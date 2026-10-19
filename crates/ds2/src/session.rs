//! Request/response exchanges over a K-line.
//!
//! The K-line is a single wire, so most cables hand back every byte we send
//! (the "echo") before the ECU's reply arrives. [`EchoMode::Auto`] copes with
//! cables that do and don't echo.
//!
//! A running engine makes the line noisy and the ECU busier, so the session
//! leaves a gap between exchanges and, after a bad frame, waits for the line
//! to go quiet before the next request.

use std::io;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::frame::{Frame, FrameError, Status, FRAME_OVERHEAD};
use crate::transport::Transport;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EchoMode {
    /// Work out from the first good exchange whether the cable echoes, then
    /// stick with that (so a noisy echo byte can't be mistaken for the reply).
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
            first_byte: Duration::from_millis(500),
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
    /// Stray bytes thrown away while waiting for the line to go quiet.
    Discard(&'a [u8]),
}

/// Default pause between the end of one exchange and the next request.
pub const DEFAULT_GAP: Duration = Duration::from_millis(20);
/// The line counts as quiet after this long without a byte.
const IDLE: Duration = Duration::from_millis(30);
/// Never spend longer than this waiting for the line to go quiet.
const MAX_DRAIN: Duration = Duration::from_millis(500);

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
    /// In `Auto` mode: whether the cable echoes, once known.
    echo_seen: Option<bool>,
    timeouts: Timeouts,
    gap: Duration,
    last_done: Option<Instant>,
    trace: Option<TraceFn>,
}

impl<T: Transport> Session<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            echo: EchoMode::Auto,
            echo_seen: None,
            timeouts: Timeouts::default(),
            gap: DEFAULT_GAP,
            last_done: None,
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

    pub fn with_gap(mut self, gap: Duration) -> Self {
        self.gap = gap;
        self
    }

    /// Changes the pause between exchanges.
    pub fn set_gap(&mut self, gap: Duration) {
        self.gap = gap;
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Sends one request and returns the reply, whatever its status.
    ///
    /// After any failure other than a dead port, waits for the line to go
    /// quiet so leftovers of a mangled reply can't spoil the next exchange.
    pub fn request(&mut self, address: u8, payload: &[u8]) -> Result<Response, SessionError> {
        if let Some(done) = self.last_done {
            let since = done.elapsed();
            if since < self.gap {
                std::thread::sleep(self.gap - since);
            }
        }
        let result = self.exchange(address, payload);
        if let Err(e) = &result {
            if !matches!(e, SessionError::Io(_)) {
                self.drain()?;
            }
        }
        self.last_done = Some(Instant::now());
        result
    }

    fn exchange(&mut self, address: u8, payload: &[u8]) -> Result<Response, SessionError> {
        let tx = Frame::new(address, payload)?.encode();
        self.transport.clear_input()?;
        self.transport.write_all(&tx)?;
        self.emit(Trace::Tx(&tx));

        let echo = match self.echo {
            EchoMode::Off => Some(false),
            EchoMode::Expect => Some(true),
            EchoMode::Auto => self.echo_seen,
        };
        let mut rx = Vec::new();
        let mut echoed = false;
        match echo {
            Some(false) => {}
            Some(true) => {
                // Drop exactly the echo, even if noise changed a byte of it.
                self.read_into(&mut rx, tx.len())?;
                if rx != tx && self.echo == EchoMode::Expect {
                    return Err(SessionError::EchoMismatch {
                        sent: crate::hex(&tx),
                        received: crate::hex(&rx),
                    });
                }
                self.emit(Trace::Echo(&rx));
                rx.clear();
                echoed = true;
            }
            None => {
                // Not known yet. Read while the bytes still match what we
                // sent, so a reply shorter than the request is never over-read.
                while rx.len() < tx.len() && tx.starts_with(&rx) {
                    let want = rx.len() + 1;
                    self.read_into(&mut rx, want)?;
                }
                if rx == tx {
                    self.emit(Trace::Echo(&rx));
                    rx.clear();
                    echoed = true;
                }
                // Otherwise what we read was the start of the reply; keep it.
            }
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
        if self.echo == EchoMode::Auto && self.echo_seen.is_none() {
            self.echo_seen = Some(echoed);
        }
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

    /// Throws bytes away until the line has been quiet for [`IDLE`].
    fn drain(&mut self) -> Result<(), SessionError> {
        let start = Instant::now();
        let mut junk = Vec::new();
        while start.elapsed() < MAX_DRAIN {
            match self.transport.read_byte(IDLE)? {
                Some(b) => junk.push(b),
                None => break,
            }
        }
        if !junk.is_empty() {
            self.emit(Trace::Discard(&junk));
        }
        self.transport.clear_input()?;
        Ok(())
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
    fn waits_the_gap_between_exchanges() {
        let mock = MockTransport::new(true)
            .expect(IDENT_REQ, reply(&[0xA0]))
            .expect(IDENT_REQ, reply(&[0xA0]));
        let mut s = Session::new(mock).with_gap(Duration::from_millis(40));
        s.request(0x12, &[0x00]).unwrap();
        let t = Instant::now();
        s.request(0x12, &[0x00]).unwrap();
        assert!(t.elapsed() >= Duration::from_millis(35));
    }

    #[test]
    fn remembers_echo_and_survives_a_corrupted_echo_byte() {
        // Cable echo simulated in the scripted bytes so it can be corrupted.
        let mut bad_echo = IDENT_REQ.to_vec();
        bad_echo[2] = 0x40;
        let mock = MockTransport::new(false)
            .expect(IDENT_REQ, [IDENT_REQ.to_vec(), reply(&[0xA0, 1])].concat())
            .expect(IDENT_REQ, [bad_echo, reply(&[0xA0, 2])].concat());
        let mut s = Session::new(mock).with_gap(Duration::ZERO);
        assert_eq!(s.request(0x12, &[0x00]).unwrap().data, vec![1]);
        assert_eq!(s.request(0x12, &[0x00]).unwrap().data, vec![2]);
    }

    #[test]
    fn drains_leftovers_after_a_bad_frame() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let mut bad = reply(&[0xA0, 0x31]);
        *bad.last_mut().unwrap() ^= 0xFF;
        bad.extend([0xEE, 0xEE]);
        let log = Rc::new(RefCell::new(Vec::new()));
        let sink = log.clone();
        let mock = MockTransport::new(true)
            .expect(IDENT_REQ, bad)
            .expect(IDENT_REQ, reply(&[0xA0]));
        let mut s = Session::new(mock)
            .with_gap(Duration::ZERO)
            .with_trace(move |t| {
                if let Trace::Discard(b) = t {
                    sink.borrow_mut().push(b.to_vec());
                }
            });
        assert!(s.request(0x12, &[0x00]).is_err());
        assert_eq!(*log.borrow(), vec![vec![0xEE, 0xEE]]);
        assert!(s.request(0x12, &[0x00]).is_ok());
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
                    Trace::Discard(b) => format!("discard {}", crate::hex(b)),
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
