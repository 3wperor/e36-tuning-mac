//! The real USB cable (K+DCAN / KKL, FTDI or CH340 based).

use std::io::{self, Read, Write};
use std::time::Duration;

use serialport::{ClearBuffer, DataBits, FlowControl, Parity, SerialPort, StopBits};

use crate::transport::Transport;

pub const DS2_BAUD: u32 = 9600;

pub use serialport::{available_ports, Error, SerialPortInfo, SerialPortType};

/// Optional control-line levels. Some cables use DTR/RTS to switch between
/// K-line and D-CAN; `None` leaves the driver default alone.
#[derive(Debug, Clone, Copy, Default)]
pub struct LineOptions {
    pub dtr: Option<bool>,
    pub rts: Option<bool>,
}

pub struct SerialTransport {
    port: Box<dyn SerialPort>,
    timeout: Duration,
}

impl SerialTransport {
    pub fn open(path: &str, lines: LineOptions) -> Result<Self, Error> {
        let timeout = Duration::from_millis(100);
        let mut port = serialport::new(path, DS2_BAUD)
            .data_bits(DataBits::Eight)
            .parity(Parity::Even)
            .stop_bits(StopBits::One)
            .flow_control(FlowControl::None)
            .timeout(timeout)
            .open()?;
        if let Some(dtr) = lines.dtr {
            port.write_data_terminal_ready(dtr)?;
        }
        if let Some(rts) = lines.rts {
            port.write_request_to_send(rts)?;
        }
        port.clear(ClearBuffer::All)?;
        Ok(Self { port, timeout })
    }
}

impl Transport for SerialTransport {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.port.write_all(bytes)?;
        self.port.flush()
    }

    fn read_byte(&mut self, timeout: Duration) -> io::Result<Option<u8>> {
        if timeout != self.timeout {
            self.port.set_timeout(timeout)?;
            self.timeout = timeout;
        }
        let mut b = [0u8; 1];
        match self.port.read(&mut b) {
            Ok(1) => Ok(Some(b[0])),
            Ok(_) => Ok(None),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn clear_input(&mut self) -> io::Result<()> {
        self.port.clear(ClearBuffer::Input).map_err(io::Error::from)
    }
}
