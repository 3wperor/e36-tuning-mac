//! A pretend MS41 for demo mode: answers ident, faults and live data with
//! values that move, so the tools can be tried without a car.

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use crate::frame::Frame;
use crate::ms41::{cmd, Flag, DME_ADDRESS, LIVE_CHANNELS, LIVE_REQUEST};
use crate::transport::Transport;

const LIVE_BLOCK_LEN: usize = 32;

/// Where the simulator puts its pretend VANOS flag. Not where a real MS41
/// keeps it; that still has to be found on the car.
pub const SIM_VANOS: Flag = Flag {
    offset: 10,
    bit: Some(3),
    invert: false,
};
/// One byte at 9600 baud, 8E1 (11 bits).
const BYTE_TIME: Duration = Duration::from_micros(1146);

pub struct SimulatedEcu {
    start: Instant,
    rx: VecDeque<u8>,
    faults: Vec<u8>,
    /// Random state for noise; `None` means a perfect line.
    noise: Option<u32>,
    /// Sleep for as long as the real wire would take.
    realtime: bool,
    /// Answer busy to requests that come sooner than this after the last one.
    needs_gap: Option<Duration>,
    last_request: Option<Instant>,
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
            noise: None,
            realtime: true,
            needs_gap: None,
            last_request: None,
        }
    }

    /// Like an ECU busy running the engine: answers busy when requests come
    /// faster than `gap` apart.
    pub fn needs_gap(mut self, gap: Duration) -> Self {
        self.needs_gap = Some(gap);
        self
    }

    /// Behaves like a running engine on a noisy line: some replies come back
    /// busy, corrupted, cut short, preceded by junk, or not at all.
    pub fn noisy(mut self, seed: u32) -> Self {
        self.noise = Some(seed.max(1));
        self
    }

    /// Answers instantly instead of at 9600-baud speed (for tests).
    pub fn fast(mut self) -> Self {
        self.realtime = false;
        self
    }

    /// Percentage roll, 0..100 (xorshift32).
    fn roll(&mut self) -> Option<u32> {
        let x = self.noise.as_mut()?;
        *x ^= *x << 13;
        *x ^= *x >> 17;
        *x ^= *x << 5;
        Some(*x % 100)
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
        // Pretend VANOS: on through the mid-range.
        if (1600.0..4200.0).contains(&rpm) {
            data[SIM_VANOS.offset] |= 1 << SIM_VANOS.bit.unwrap_or(0);
        }
        // A few other status bits that flicker, so finding VANOS takes a look.
        data[10] |= ((t * 3.0) as u8 & 1) << 6;
        data[11] = (t * 0.5) as u8 & 0b101;
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
            let too_soon = self
                .needs_gap
                .zip(self.last_request)
                .is_some_and(|(gap, last)| last.elapsed() < gap);
            self.last_request = Some(Instant::now());
            if req.address == DME_ADDRESS && too_soon {
                let busy = Frame::new(DME_ADDRESS, vec![0xA1]).unwrap().encode();
                self.rx.extend(busy);
            } else if req.address == DME_ADDRESS {
                let reply = self.reply_to(&req.payload);
                let mut frame = Frame::new(DME_ADDRESS, reply)
                    .expect("reply fits in a frame")
                    .encode();
                match self.roll() {
                    Some(0..8) => {
                        frame = Frame::new(DME_ADDRESS, vec![0xA1]).unwrap().encode();
                    }
                    Some(r @ 8..14) => {
                        let i = r as usize % frame.len();
                        frame[i] ^= 0x5A;
                    }
                    Some(14..18) => frame.clear(),
                    Some(18..22) => frame.truncate(frame.len() / 2),
                    Some(22..25) => frame.insert(0, 0xFF),
                    _ => {}
                }
                self.rx.extend(frame);
            }
        }
        if self.realtime {
            // Take as long as the real wire would, so demo mode polls at a
            // realistic rate instead of spinning.
            std::thread::sleep(BYTE_TIME * self.rx.len() as u32);
        }
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
        let mut s = Session::new(SimulatedEcu::new().fast());
        let id = ms41::ident(&mut s).unwrap();
        assert_eq!(id.part_number.as_deref(), Some("1429861"));

        let live = ms41::read_live(&mut s).unwrap();
        let rpm = LIVE_CHANNELS[0].decode(&live).unwrap();
        assert!((700.0..=4500.0).contains(&rpm), "rpm {rpm}");
        assert_eq!(
            SIM_VANOS.decode(&live),
            Some((1600.0..4200.0).contains(&rpm))
        );

        assert!(!ms41::read_faults(&mut s).unwrap().raw.is_empty());
        ms41::clear_faults(&mut s).unwrap();
        assert_eq!(ms41::read_faults(&mut s).unwrap().raw, vec![0x00]);
    }
}
