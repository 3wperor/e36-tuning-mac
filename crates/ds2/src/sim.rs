//! A pretend MS41 for demo mode: answers ident, faults and live data with
//! values that move, so the tools can be tried without a car.

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use crate::frame::Frame;
use crate::ms41::{cmd, DME_ADDRESS, LIVE_CHANNELS, LIVE_REQUEST};
use crate::transport::Transport;

const LIVE_BLOCK_LEN: usize = 32;
/// One byte at 9600 baud, 8E1 (11 bits).
const BYTE_TIME: Duration = Duration::from_micros(1146);

pub struct SimulatedEcu {
    start: Instant,
    rx: VecDeque<u8>,
    faults: Vec<u8>,
}

impl Default for SimulatedEcu {
    fn default() -> Self {
        Self::new()
    }
}

impl SimulatedEcu {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            rx: VecDeque::new(),
            // Coolant temperature sensor and knock sensor bank 1, laid out as
            // count + 8-byte records.
            faults: vec![
                0x02, 10, 0x21, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 57, 0x11, 0x01, 0x00, 0x00,
                0x00, 0x00, 0x00,
            ],
        }
    }

    fn live_block(&self) -> Vec<u8> {
        let t = self.start.elapsed().as_secs_f64();
        let rpm = 800.0 + (1.0 - (t * 0.6).cos()) * 1800.0;
        let coolant = 20.0 + 70.0 * (1.0 - (-t / 40.0).exp());
        let values = [rpm, 24.0 + (t * 0.1).sin(), coolant, 8.0 + rpm / 250.0];
        let mut data = vec![0u8; LIVE_BLOCK_LEN];
        for (ch, v) in LIVE_CHANNELS.iter().zip(values) {
            ch.encode(v, &mut data);
        }
        data
    }

    fn reply_to(&mut self, payload: &[u8]) -> Vec<u8> {
        match payload {
            [cmd::IDENT] => [&[0xA0][..], b"1429861", &[0x00, 0x00]].concat(),
            [cmd::READ_FAULTS] | [cmd::READ_FAULTS, 0x01] => [&[0xA0][..], &self.faults].concat(),
            [cmd::CLEAR_FAULTS] => {
                self.faults = vec![0x00];
                vec![0xA0]
            }
            p if p == LIVE_REQUEST => [vec![0xA0], self.live_block()].concat(),
            _ => vec![0xA2],
        }
    }
}

impl Transport for SimulatedEcu {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        // Like a real K-line, everything sent comes back first.
        self.rx.extend(bytes);
        if let Ok(req) = Frame::decode(bytes) {
            if req.address == DME_ADDRESS {
                let reply = self.reply_to(&req.payload);
                let frame = Frame::new(DME_ADDRESS, reply).expect("reply fits in a frame");
                self.rx.extend(frame.encode());
            }
        }
        // Take as long as the real wire would, so demo mode polls at a
        // realistic rate instead of spinning.
        std::thread::sleep(BYTE_TIME * self.rx.len() as u32);
        Ok(())
    }

    fn read_byte(&mut self, _timeout: Duration) -> io::Result<Option<u8>> {
        Ok(self.rx.pop_front())
    }

    fn clear_input(&mut self) -> io::Result<()> {
        self.rx.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ms41;
    use crate::session::Session;

    #[test]
    fn answers_ident_live_and_faults() {
        let mut s = Session::new(SimulatedEcu::new());
        let id = ms41::ident(&mut s).unwrap();
        assert_eq!(id.part_number.as_deref(), Some("1429861"));

        let live = ms41::read_live(&mut s).unwrap();
        let rpm = LIVE_CHANNELS[0].decode(&live).unwrap();
        assert!((700.0..=4500.0).contains(&rpm), "rpm {rpm}");

        assert!(!ms41::read_faults(&mut s).unwrap().raw.is_empty());
        ms41::clear_faults(&mut s).unwrap();
        assert_eq!(ms41::read_faults(&mut s).unwrap().raw, vec![0x00]);
    }
}
