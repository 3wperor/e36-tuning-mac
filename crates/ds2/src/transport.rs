//! The byte pipe a [`Session`](crate::Session) talks through.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

pub trait Transport {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// Reads one byte, waiting at most `timeout`. `Ok(None)` means timed out.
    fn read_byte(&mut self, timeout: Duration) -> io::Result<Option<u8>>;
    /// Drops anything sitting in the receive buffer.
    fn clear_input(&mut self) -> io::Result<()>;
}

/// A scripted fake ECU for tests: each write must match the next expected
/// request, after which its reply (preceded by the echo, if enabled) becomes
/// readable.
#[derive(Debug, Default)]
pub struct MockTransport {
    echo: bool,
    script: VecDeque<(Vec<u8>, Vec<u8>)>,
    rx: VecDeque<u8>,
    pub written: Vec<Vec<u8>>,
}

impl MockTransport {
    /// `echo` mimics a real K-line, where every byte sent is also received.
    pub fn new(echo: bool) -> Self {
        Self {
            echo,
            ..Self::default()
        }
    }

    pub fn expect(mut self, request: impl Into<Vec<u8>>, reply: impl Into<Vec<u8>>) -> Self {
        self.script.push_back((request.into(), reply.into()));
        self
    }

    pub fn is_finished(&self) -> bool {
        self.script.is_empty()
    }
}

impl Transport for MockTransport {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.written.push(bytes.to_vec());
        if self.echo {
            self.rx.extend(bytes);
        }
        match self.script.pop_front() {
            Some((expected, reply)) if expected == bytes => {
                self.rx.extend(reply);
                Ok(())
            }
            Some((expected, _)) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "mock expected {} but got {}",
                    crate::hex(&expected),
                    crate::hex(bytes)
                ),
            )),
            // Nothing scripted: behave like a silent ECU.
            None => Ok(()),
        }
    }

    fn read_byte(&mut self, _timeout: Duration) -> io::Result<Option<u8>> {
        Ok(self.rx.pop_front())
    }

    fn clear_input(&mut self) -> io::Result<()> {
        self.rx.clear();
        Ok(())
    }
}
