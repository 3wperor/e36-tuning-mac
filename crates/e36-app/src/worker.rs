//! Background thread that owns the ECU connection, so the window never
//! freezes while waiting on the 9600-baud K-line.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use ds2::serial::{LineOptions, SerialTransport};
use ds2::{ms41, Session, SessionError, SimulatedEcu, Transport};

/// Give up after this many failed live reads in a row.
const MAX_CONSECUTIVE_ERRORS: u32 = 5;

pub enum Source {
    Demo,
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

fn run(source: Source, commands: &Receiver<Command>, send: &dyn Fn(Event)) -> Result<(), String> {
    let transport: Box<dyn Transport> = match source {
        Source::Demo => Box::new(SimulatedEcu::new()),
        Source::Cable(path) => Box::new(
            SerialTransport::open(&path, LineOptions::default())
                .map_err(|e| format!("could not open {path}: {e}"))?,
        ),
    };
    let mut session = Session::new(transport);
    let ident = ms41::ident(&mut session).map_err(|e| e.to_string())?;
    send(Event::Connected(ident));

    let mut live_supported = true;
    let mut errors = 0;
    loop {
        match commands.try_recv() {
            Ok(Command::Disconnect) | Err(TryRecvError::Disconnected) => return Ok(()),
            Ok(Command::ReadFaults) => send(match ms41::read_faults(&mut session) {
                Ok(f) => Event::Faults(f.raw),
                Err(e) => Event::Error(format!("reading faults: {e}")),
            }),
            Ok(Command::ClearFaults) => send(match ms41::clear_faults(&mut session) {
                Ok(()) => Event::FaultsCleared,
                Err(e) => Event::Error(format!("clearing faults: {e}")),
            }),
            Err(TryRecvError::Empty) => {}
        }

        if !live_supported {
            thread::sleep(Duration::from_millis(100));
            continue;
        }
        match ms41::read_live(&mut session) {
            Ok(data) => {
                errors = 0;
                send(Event::Live(Instant::now(), data));
            }
            Err(SessionError::Negative(status)) => {
                live_supported = false;
                send(Event::LiveUnsupported(format!(
                    "the ECU answered {status} to the live data request"
                )));
            }
            Err(e) => {
                errors += 1;
                if errors >= MAX_CONSECUTIVE_ERRORS {
                    return Err(format!("lost connection: {e}"));
                }
                send(Event::Error(e.to_string()));
            }
        }
    }
}
