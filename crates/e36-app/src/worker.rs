//! Background thread that owns the ECU connection, so the window never
//! freezes while waiting on the 9600-baud K-line.
//!
//! Once connected it keeps going: live reads are paced by [`LivePoller`],
//! failures are retried, and if the link really goes (cable pulled, or no
//! good reply for a while) it reopens the port and carries on.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use ds2::serial::{LineOptions, SerialTransport};
use ds2::{
    ms41, LinkStats, LivePoller, Pacing, Session, SessionError, SimulatedEcu, Status, Transport,
};

/// Reopen the port after this many failed live reads in a row. With the
/// poller backing off, that's several seconds without a single good reply.
const RECONNECT_AFTER: u32 = 15;
/// Pause before trying to reconnect.
const RECONNECT_DELAY: Duration = Duration::from_secs(1);
/// How often link stats are sent to the UI.
const STATS_EVERY: Duration = Duration::from_millis(500);
/// How often the knock retard group is read, between live reads.
const KNOCK_EVERY: Duration = Duration::from_secs(1);
/// How often the fault memory is re-read to catch new codes (misfire, knock
/// sensor, ...) while logging.
const FAULTS_EVERY: Duration = Duration::from_secs(15);
/// Outright refusals of the knock request, before it ever worked, that mean
/// this ECU doesn't support it.
const KNOCK_REFUSALS: u32 = 3;
/// Attempts at reading the ident before a connection counts as failed.
const IDENT_ATTEMPTS: u32 = 3;

#[derive(Clone)]
pub enum Source {
    Demo { noisy: bool },
    Cable(String),
}

pub enum Command {
    ReadFaults,
    ClearFaults,
    Disconnect,
}

pub enum Event {
    Connected(ms41::Ident),
    Live(Instant, Vec<u8>),
    LiveUnsupported(String),
    Knock(Instant, Vec<u8>),
    KnockUnsupported(String),
    Link(LinkStats),
    Reconnecting(String),
    Faults(Vec<u8>),
    FaultsCleared,
    Error(String),
    Disconnected(Option<String>),
}

pub struct Worker {
    commands: Sender<Command>,
    pub events: Receiver<Event>,
}

impl Worker {
    /// `repaint` is called after every event so the UI wakes up.
    pub fn spawn(source: Source, repaint: impl Fn() + Send + 'static) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        thread::spawn(move || {
            let send = |e: Event| {
                let _ = ev_tx.send(e);
                repaint();
            };
            let reason = run(source, &cmd_rx, &send).err();
            send(Event::Disconnected(reason));
        });
        Self {
            commands: cmd_tx,
            events: ev_rx,
        }
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.send(Command::Disconnect);
    }
}

/// Why the polling loop stopped.
enum Exit {
    Quit,
    Reconnect(String),
}

type Opener<'a> = dyn Fn() -> Result<Box<dyn Transport>, String> + 'a;

fn run(source: Source, commands: &Receiver<Command>, send: &dyn Fn(Event)) -> Result<(), String> {
    run_with(&|| open(&source), commands, send, RECONNECT_DELAY)
}

fn open(source: &Source) -> Result<Box<dyn Transport>, String> {
    Ok(match source {
        Source::Demo { noisy: false } => Box::new(SimulatedEcu::new()),
        Source::Demo { noisy: true } => Box::new(
            SimulatedEcu::new()
                .noisy(0x5eed)
                .needs_gap(Duration::from_millis(60)),
        ),
        Source::Cable(path) => Box::new(
            SerialTransport::open(path, LineOptions::default())
                .map_err(|e| format!("could not open {path}: {e}"))?,
        ),
    })
}

/// Runs until the user disconnects. Only a failure to connect the very first
/// time is reported as an error; after that it keeps reconnecting.
fn run_with(
    open: &Opener<'_>,
    commands: &Receiver<Command>,
    send: &dyn Fn(Event),
    reconnect_delay: Duration,
) -> Result<(), String> {
    let mut session = connect(open, send)?;
    loop {
        match poll_loop(&mut session, commands, send) {
            Exit::Quit => return Ok(()),
            Exit::Reconnect(reason) => {
                drop(session); // close the port before reopening it
                send(Event::Reconnecting(reason));
                loop {
                    if wait_or_quit(commands, reconnect_delay) {
                        return Ok(());
                    }
                    match connect(open, send) {
                        Ok(s) => {
                            session = s;
                            break;
                        }
                        Err(e) => send(Event::Reconnecting(e)),
                    }
                }
            }
        }
    }
}

fn connect(open: &Opener<'_>, send: &dyn Fn(Event)) -> Result<Session<Box<dyn Transport>>, String> {
    let mut session = Session::new(open()?);
    let mut last_err = None;
    for _ in 0..IDENT_ATTEMPTS {
        match ms41::ident(&mut session) {
            Ok(ident) => {
                send(Event::Connected(ident));
                return Ok(session);
            }
            Err(e @ SessionError::Io(_)) => return Err(e.to_string()),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.map_or_else(String::new, |e| e.to_string()))
}

fn poll_loop(
    session: &mut Session<Box<dyn Transport>>,
    commands: &Receiver<Command>,
    send: &dyn Fn(Event),
) -> Exit {
    let mut poller = LivePoller::new(Pacing::default());
    let mut live_paused = false;
    let mut last_stats = Instant::now();
    let mut last_knock: Option<Instant> = None;
    let mut knock = KnockState::Trying { refusals: 0 };
    // `None`: check the fault memory straight away for a baseline.
    let mut last_faults: Option<Instant> = None;
    loop {
        match commands.try_recv() {
            Ok(Command::Disconnect) | Err(TryRecvError::Disconnected) => return Exit::Quit,
            Ok(Command::ReadFaults) => send(match ms41::read_faults(session) {
                Ok(f) => Event::Faults(f.raw),
                Err(e) => Event::Error(format!("reading faults: {e}")),
            }),
            Ok(Command::ClearFaults) => send(match ms41::clear_faults(session) {
                Ok(()) => Event::FaultsCleared,
                Err(e) => Event::Error(format!("clearing faults: {e}")),
            }),
            Err(TryRecvError::Empty) => {}
        }

        if live_paused {
            thread::sleep(Duration::from_millis(100));
            continue;
        }
        match poller.poll(session) {
            Ok(data) => send(Event::Live(Instant::now(), data)),
            Err(SessionError::Io(e)) => return Exit::Reconnect(format!("cable problem: {e}")),
            Err(e) => {
                if poller.looks_unsupported() {
                    live_paused = true;
                    send(Event::LiveUnsupported(e.to_string()));
                } else if poller.stats().consecutive_failures >= RECONNECT_AFTER {
                    return Exit::Reconnect(format!("no good replies for a while ({e})"));
                }
            }
        }

        if knock != KnockState::Unsupported && last_knock.is_none_or(|t| t.elapsed() >= KNOCK_EVERY)
        {
            last_knock = Some(Instant::now());
            match poller.run(session, ms41::read_knock) {
                Ok(data) => {
                    knock = KnockState::Working;
                    send(Event::Knock(Instant::now(), data));
                }
                Err(SessionError::Io(e)) => return Exit::Reconnect(format!("cable problem: {e}")),
                Err(SessionError::Negative(status)) if status != Status::Busy => {
                    if let KnockState::Trying { refusals } = &mut knock {
                        *refusals += 1;
                        if *refusals >= KNOCK_REFUSALS {
                            knock = KnockState::Unsupported;
                            send(Event::KnockUnsupported(format!(
                                "the ECU answered {status} to the knock request"
                            )));
                        }
                    }
                }
                Err(_) => {}
            }
        }

        if last_faults.is_none_or(|t| t.elapsed() >= FAULTS_EVERY) {
            last_faults = Some(Instant::now());
            match poller.run(session, ms41::read_faults) {
                Ok(f) => send(Event::Faults(f.raw)),
                Err(SessionError::Io(e)) => return Exit::Reconnect(format!("cable problem: {e}")),
                // Try again at the next check.
                Err(_) => {}
            }
        }

        if last_stats.elapsed() >= STATS_EVERY {
            last_stats = Instant::now();
            send(Event::Link(poller.stats().clone()));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KnockState {
    /// Not answered yet; counting outright refusals.
    Trying {
        refusals: u32,
    },
    Working,
    Unsupported,
}

/// Sleeps for `delay`, returning early with `true` if the user disconnects.
fn wait_or_quit(commands: &Receiver<Command>, delay: Duration) -> bool {
    match commands.recv_timeout(delay) {
        Ok(Command::Disconnect) | Err(mpsc::RecvTimeoutError::Disconnected) => true,
        Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::io;

    /// A simulated ECU whose cable "unplugs" after a number of writes.
    struct Unplugs {
        ecu: SimulatedEcu,
        writes_left: u32,
    }

    impl Transport for Unplugs {
        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            if self.writes_left == 0 {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "device gone"));
            }
            self.writes_left -= 1;
            self.ecu.write_all(bytes)
        }
        fn read_byte(&mut self, t: Duration) -> io::Result<Option<u8>> {
            self.ecu.read_byte(t)
        }
        fn clear_input(&mut self) -> io::Result<()> {
            self.ecu.clear_input()
        }
    }

    #[test]
    fn reconnects_after_the_cable_drops_and_keeps_streaming() {
        let opened = Cell::new(0);
        let open = || -> Result<Box<dyn Transport>, String> {
            opened.set(opened.get() + 1);
            Ok(Box::new(Unplugs {
                ecu: SimulatedEcu::new().fast(),
                writes_left: 6,
            }))
        };
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let events = RefCell::new(Vec::new());
        let live = Cell::new(0);
        let send = |e: Event| {
            if let Event::Live(..) = e {
                live.set(live.get() + 1);
                // Stop once data has flowed across a couple of reconnects.
                if live.get() == 12 {
                    let _ = cmd_tx.send(Command::Disconnect);
                }
            }
            events.borrow_mut().push(match e {
                Event::Connected(_) => "connected",
                Event::Reconnecting(_) => "reconnecting",
                _ => "other",
            });
        };
        let result = run_with(&open, &cmd_rx, &send, Duration::from_millis(1));
        assert!(result.is_ok());
        assert_eq!(live.get(), 12);
        assert!(opened.get() >= 3, "opened {} times", opened.get());
        let events = events.borrow();
        assert!(events.iter().filter(|e| **e == "reconnecting").count() >= 2);
        assert!(events.iter().filter(|e| **e == "connected").count() >= 3);
    }

    #[test]
    fn first_connection_failure_is_reported() {
        let open = || -> Result<Box<dyn Transport>, String> { Err("no such port".into()) };
        let (_tx, rx) = mpsc::channel();
        let err = run_with(&open, &rx, &|_| {}, Duration::from_millis(1)).unwrap_err();
        assert_eq!(err, "no such port");
    }
}
