//! Keeps live data flowing from a running engine.
//!
//! With the engine running, the ECU has less time for diagnostics and the
//! line picks up electrical noise. [`LivePoller`] paces requests so the ECU
//! can keep up, judging by the recent *rate* of "busy" and missing replies
//! rather than by single failures: a burst of them slows it down, a quiet
//! spell speeds it back up. Corrupted replies are noise, not a sign the ECU
//! needs more time, so they don't change the pace. Failures are counted,
//! never fatal; the caller decides when a run of them means the link is gone.

use std::collections::VecDeque;
use std::time::Duration;

use crate::frame::Status;
use crate::ms41;
use crate::session::{Session, SessionError};
use crate::transport::Transport;

#[derive(Debug, Clone, Copy)]
pub struct Pacing {
    /// Fastest the poller will go.
    pub min_gap: Duration,
    /// Where it starts.
    pub start_gap: Duration,
    /// Slowest it will back off to.
    pub max_gap: Duration,
    /// How many recent replies the rate is judged over.
    pub window: usize,
    /// Re-judge the pace after this many requests.
    pub adjust_every: u32,
    /// Slow down when more than this share of recent replies were busy or missing.
    pub slow_down_above: f64,
    /// Speed up when fewer than this share were.
    pub speed_up_below: f64,
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            min_gap: Duration::from_millis(20),
            start_gap: Duration::from_millis(50),
            max_gap: Duration::from_millis(500),
            window: 20,
            adjust_every: 10,
            slow_down_above: 0.35,
            speed_up_below: 0.2,
        }
    }
}

/// Busy or missing replies in a row that slow it down straight away.
const SLOW_RUN: u32 = 3;

/// How the link is doing, for display.
#[derive(Debug, Clone, Default)]
pub struct LinkStats {
    pub good: u64,
    pub failed: u64,
    pub consecutive_failures: u32,
    /// Current pause between requests.
    pub gap: Duration,
    pub last_error: Option<String>,
}

impl LinkStats {
    /// Share of requests that got a good reply, 0.0 to 1.0.
    pub fn quality(&self) -> f64 {
        let total = self.good + self.failed;
        if total == 0 {
            1.0
        } else {
            self.good as f64 / total as f64
        }
    }
}

pub struct LivePoller {
    pacing: Pacing,
    gap: Duration,
    /// Recent requests: `true` where the ECU was busy or didn't answer.
    recent: VecDeque<bool>,
    since_adjust: u32,
    slow_run: u32,
    ever_ok: bool,
    refusals: u32,
    stats: LinkStats,
}

impl LivePoller {
    pub fn new(pacing: Pacing) -> Self {
        Self {
            pacing,
            gap: pacing.start_gap,
            recent: VecDeque::with_capacity(pacing.window),
            since_adjust: 0,
            slow_run: 0,
            ever_ok: false,
            refusals: 0,
            stats: LinkStats {
                gap: pacing.start_gap,
                ..LinkStats::default()
            },
        }
    }

    pub fn stats(&self) -> &LinkStats {
        &self.stats
    }

    /// One attempt at reading the live data block. Adjusts the pacing from
    /// the result; an error here is usually worth simply trying again.
    pub fn poll<T: Transport>(
        &mut self,
        session: &mut Session<T>,
    ) -> Result<Vec<u8>, SessionError> {
        let result = self.run(session, ms41::read_live);
        match &result {
            Ok(_) => self.ever_ok = true,
            Err(SessionError::Negative(status)) if *status != Status::Busy => self.refusals += 1,
            Err(_) => {}
        }
        result
    }

    /// Runs any other request (knock, faults, ...) under the same pacing, so
    /// extra requests interleaved with live data don't overload the ECU.
    pub fn run<T: Transport, R>(
        &mut self,
        session: &mut Session<T>,
        request: impl FnOnce(&mut Session<T>) -> Result<R, SessionError>,
    ) -> Result<R, SessionError> {
        session.set_gap(self.gap);
        let result = request(session);
        let mut needed_time = false;
        match &result {
            Ok(_) => {
                self.stats.good += 1;
                self.stats.consecutive_failures = 0;
            }
            Err(e) => {
                self.stats.failed += 1;
                self.stats.consecutive_failures += 1;
                self.stats.last_error = Some(e.to_string());
                needed_time = matches!(
                    e,
                    SessionError::Negative(Status::Busy) | SessionError::NoResponse
                );
            }
        }
        self.adjust(needed_time);
        self.stats.gap = self.gap;
        result
    }

    fn adjust(&mut self, needed_time: bool) {
        if self.recent.len() == self.pacing.window {
            self.recent.pop_front();
        }
        self.recent.push_back(needed_time);
        self.slow_run = if needed_time { self.slow_run + 1 } else { 0 };
        self.since_adjust += 1;

        if self.slow_run >= SLOW_RUN {
            // Sudden overload: don't wait for the window.
            self.slow_down();
            return;
        }
        if self.since_adjust >= self.pacing.adjust_every {
            self.since_adjust = 0;
            let rate = self.recent.iter().filter(|b| **b).count() as f64 / self.recent.len() as f64;
            if rate > self.pacing.slow_down_above {
                self.slow_down();
            } else if rate < self.pacing.speed_up_below {
                self.gap = (self.gap * 9 / 10).max(self.pacing.min_gap);
            }
        }
    }

    fn slow_down(&mut self) {
        self.gap = (self.gap * 3 / 2).min(self.pacing.max_gap);
        self.recent.clear();
        self.since_adjust = 0;
        self.slow_run = 0;
    }

    /// True when the ECU has refused the live data request outright several
    /// times and never answered it, as opposed to struggling to keep up.
    pub fn looks_unsupported(&self) -> bool {
        !self.ever_ok && self.refusals >= 3
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Frame;
    use crate::sim::SimulatedEcu;
    use crate::transport::MockTransport;

    const LIVE_REQ: [u8; 5] = [0x12, 0x05, 0x0B, 0x03, 0x1F];

    fn reply(payload: &[u8]) -> Vec<u8> {
        Frame::new(0x12, payload).unwrap().encode()
    }

    fn quick() -> Pacing {
        Pacing {
            min_gap: Duration::from_millis(1),
            start_gap: Duration::from_millis(4),
            max_gap: Duration::from_millis(16),
            window: 10,
            adjust_every: 5,
            ..Pacing::default()
        }
    }

    #[test]
    fn keeps_going_on_a_noisy_line() {
        let mut s = Session::new(SimulatedEcu::new().fast().noisy(7));
        let mut poller = LivePoller::new(quick());
        let mut good = 0;
        for _ in 0..300 {
            if poller.poll(&mut s).is_ok() {
                good += 1;
            }
        }
        let stats = poller.stats();
        assert_eq!(stats.good + stats.failed, 300);
        assert_eq!(stats.good, good);
        assert!(stats.quality() > 0.6, "quality {}", stats.quality());
        assert!(stats.failed > 0, "noise should cause some failures");
        assert!(!poller.looks_unsupported());
    }

    #[test]
    fn a_run_of_busy_replies_slows_it_down_at_once() {
        let mut mock = MockTransport::new(true);
        for _ in 0..3 {
            mock = mock.expect(LIVE_REQ, reply(&[0xA1]));
        }
        let mut s = Session::new(mock);
        let mut poller = LivePoller::new(quick());
        for _ in 0..3 {
            let _ = poller.poll(&mut s);
        }
        assert_eq!(poller.stats().gap, Duration::from_millis(6));
    }

    #[test]
    fn corrupted_replies_do_not_change_the_pace() {
        let mut corrupted = reply(&[0xA0, 1]);
        *corrupted.last_mut().unwrap() ^= 0xFF;
        let mut mock = MockTransport::new(true);
        for _ in 0..4 {
            mock = mock.expect(LIVE_REQ, corrupted.clone());
        }
        let mut s = Session::new(mock);
        let mut poller = LivePoller::new(quick());
        for _ in 0..4 {
            assert!(poller.poll(&mut s).is_err());
        }
        assert_eq!(poller.stats().gap, quick().start_gap);
        assert_eq!(poller.stats().consecutive_failures, 4);
    }

    #[test]
    fn a_clean_spell_speeds_it_up() {
        let mut mock = MockTransport::new(true);
        for _ in 0..5 {
            mock = mock.expect(LIVE_REQ, reply(&[0xA0, 1]));
        }
        let mut s = Session::new(mock);
        let mut poller = LivePoller::new(quick());
        for _ in 0..5 {
            poller.poll(&mut s).unwrap();
        }
        assert_eq!(poller.stats().gap, Duration::from_micros(3600));
    }

    #[test]
    fn settles_at_the_pace_a_busy_ecu_can_handle() {
        let mut s = Session::new(
            SimulatedEcu::new()
                .fast()
                .needs_gap(Duration::from_millis(6)),
        );
        let mut poller = LivePoller::new(quick());
        for _ in 0..200 {
            let _ = poller.poll(&mut s);
        }
        let stats = poller.stats();
        assert!(stats.quality() > 0.6, "quality {}", stats.quality());
        assert!(
            stats.gap >= Duration::from_millis(4),
            "gap {:?} should have grown to suit the ECU",
            stats.gap
        );
    }

    #[test]
    fn random_noise_keeps_the_pace_near_full_speed() {
        let mut s = Session::new(SimulatedEcu::new().fast().noisy(99));
        let mut poller = LivePoller::new(quick());
        for _ in 0..300 {
            let _ = poller.poll(&mut s);
        }
        assert!(
            poller.stats().gap <= Duration::from_millis(4),
            "gap {:?}",
            poller.stats().gap
        );
    }

    #[test]
    fn gap_stays_within_limits() {
        let mut mock = MockTransport::new(true);
        for _ in 0..20 {
            mock = mock.expect(LIVE_REQ, reply(&[0xA1]));
        }
        let mut s = Session::new(mock);
        let mut poller = LivePoller::new(quick());
        for _ in 0..20 {
            let _ = poller.poll(&mut s);
        }
        assert_eq!(poller.stats().gap, quick().max_gap);
        assert_eq!(poller.stats().consecutive_failures, 20);
    }

    #[test]
    fn busy_is_never_taken_as_unsupported() {
        let mut mock = MockTransport::new(true);
        for _ in 0..5 {
            mock = mock.expect(LIVE_REQ, reply(&[0xA1]));
        }
        let mut s = Session::new(mock);
        let mut poller = LivePoller::new(quick());
        for _ in 0..5 {
            let _ = poller.poll(&mut s);
        }
        assert!(!poller.looks_unsupported());
    }

    #[test]
    fn repeated_refusals_before_any_success_look_unsupported() {
        let mut mock = MockTransport::new(true);
        for _ in 0..3 {
            mock = mock.expect(LIVE_REQ, reply(&[0xA2]));
        }
        let mut s = Session::new(mock);
        let mut poller = LivePoller::new(quick());
        for _ in 0..3 {
            let _ = poller.poll(&mut s);
        }
        assert!(poller.looks_unsupported());
    }

    #[test]
    fn refusals_after_live_data_worked_are_not_unsupported() {
        let mut mock = MockTransport::new(true).expect(LIVE_REQ, reply(&[0xA0, 1]));
        for _ in 0..3 {
            mock = mock.expect(LIVE_REQ, reply(&[0xA2]));
        }
        let mut s = Session::new(mock);
        let mut poller = LivePoller::new(quick());
        for _ in 0..4 {
            let _ = poller.poll(&mut s);
        }
        assert!(!poller.looks_unsupported());
    }
}
