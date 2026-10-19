use std::io::{self, BufRead, Write};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use ds2::serial::{available_ports, LineOptions, SerialPortType, SerialTransport};
use ds2::{hex, ms41, EchoMode, Session, SimulatedEcu, Trace, Transport};

/// Diagnostics for the BMW E36 Siemens MS41 DME over a K+DCAN cable.
///
/// Turn the ignition to position II (engine off) before running a command.
#[derive(Parser)]
#[command(name = "e36", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Serial port of the cable, e.g. /dev/cu.usbserial-A50285BI. Auto-detected if omitted.
    #[arg(short, long, global = true, env = "E36_PORT")]
    port: Option<String>,

    /// Print every byte sent and received.
    #[arg(long, global = true)]
    trace: bool,

    /// How to handle the K-line echo (most cables echo every byte sent).
    #[arg(long, global = true, value_enum, default_value_t = Echo::Auto)]
    echo: Echo,

    /// Force the DTR line level (some cables use it to select K-line mode).
    #[arg(long, global = true, value_enum)]
    dtr: Option<Level>,

    /// Force the RTS line level.
    #[arg(long, global = true, value_enum)]
    rts: Option<Level>,

    /// Talk to a simulated ECU instead of the car (no cable needed).
    #[arg(long, global = true)]
    demo: bool,
}

#[derive(Subcommand)]
enum Command {
    /// List serial ports and highlight the likely cable.
    Ports,
    /// Read the DME's identification (part number etc.).
    Ident,
    /// Read the stored fault codes.
    Faults,
    /// Stream live data until Ctrl-C. Decoded values are unverified.
    Live {
        /// Also print the raw data block.
        #[arg(long)]
        raw: bool,
    },
    /// Read, then erase, the stored fault codes.
    ClearFaults {
        /// Don't ask for confirmation.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Echo {
    Auto,
    On,
    Off,
}

#[derive(Clone, Copy, ValueEnum)]
enum Level {
    High,
    Low,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Ports => list_ports(),
        Command::Ident => {
            let id = ms41::ident(&mut open(&cli)?)?;
            println!(
                "DME part number : {}",
                id.part_number_pretty()
                    .as_deref()
                    .unwrap_or("(not recognised)")
            );
            println!("Ident (text)    : {}", id.ascii());
            println!("Ident (hex)     : {}", hex(&id.raw));
            Ok(())
        }
        Command::Faults => {
            let faults = ms41::read_faults(&mut open(&cli)?)?;
            print_faults(&faults);
            Ok(())
        }
        Command::Live { raw } => {
            let mut session = open(&cli)?;
            loop {
                let data = ms41::read_live(&mut session)?;
                let line: Vec<String> = ms41::LIVE_CHANNELS
                    .iter()
                    .map(|ch| match ch.decode(&data) {
                        Some(v) => format!("{} {v:.1} {}", ch.name, ch.unit),
                        None => format!("{} -", ch.name),
                    })
                    .collect();
                println!("{}", line.join(" | "));
                if raw {
                    println!("  {}", hex(&data));
                }
            }
        }
        Command::ClearFaults { yes } => {
            let mut session = open(&cli)?;
            let faults = ms41::read_faults(&mut session)?;
            print_faults(&faults);
            if !yes && !confirm("Erase these fault codes from the DME?")? {
                println!("Nothing changed.");
                return Ok(());
            }
            ms41::clear_faults(&mut session)?;
            println!("Fault memory cleared.");
            Ok(())
        }
    }
}

fn open(cli: &Cli) -> Result<Session<Box<dyn Transport>>> {
    let transport: Box<dyn Transport> = if cli.demo {
        eprintln!("Using simulated ECU (demo mode)");
        Box::new(SimulatedEcu::new())
    } else {
        Box::new(open_cable(cli)?)
    };
    let mut session = Session::new(transport).with_echo(match cli.echo {
        Echo::Auto => EchoMode::Auto,
        Echo::On => EchoMode::Expect,
        Echo::Off => EchoMode::Off,
    });
    if cli.trace {
        session = session.with_trace(|t| match t {
            Trace::Tx(b) => eprintln!("  -> {}", hex(b)),
            Trace::Echo(b) => eprintln!("  (echo) {}", hex(b)),
            Trace::Rx(b) => eprintln!("  <- {}", hex(b)),
        });
    }
    Ok(session)
}

fn open_cable(cli: &Cli) -> Result<SerialTransport> {
    let path = match &cli.port {
        Some(p) => p.clone(),
        None => pick_port(&candidates()?)?,
    };
    eprintln!("Using {path}");
    let lines = LineOptions {
        dtr: cli.dtr.map(|l| matches!(l, Level::High)),
        rts: cli.rts.map(|l| matches!(l, Level::High)),
    };
    SerialTransport::open(&path, lines).with_context(|| format!("could not open {path}"))
}

fn print_faults(faults: &ms41::FaultMemory) {
    println!("Fault memory ({} bytes):", faults.raw.len());
    for chunk in faults.raw.chunks(16) {
        println!("  {}", hex(chunk));
    }
    println!("(Decoding MS41 fault codes into text is not implemented yet; the raw bytes above are what the DME sent.)");
}

fn confirm(question: &str) -> Result<bool> {
    print!("{question} Type 'yes' to continue: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    Ok(answer.trim().eq_ignore_ascii_case("yes"))
}

/// A serial port as far as cable detection cares.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    name: String,
    usb: bool,
    description: String,
}

fn candidates() -> Result<Vec<Candidate>> {
    let ports = available_ports().context("could not list serial ports")?;
    Ok(ports
        .into_iter()
        .map(|p| {
            let (usb, description) = match &p.port_type {
                SerialPortType::UsbPort(u) => (
                    true,
                    format!(
                        "USB {:04x}:{:04x} {} {}",
                        u.vid,
                        u.pid,
                        u.manufacturer.as_deref().unwrap_or(""),
                        u.product.as_deref().unwrap_or("")
                    )
                    .trim_end()
                    .to_string(),
                ),
                SerialPortType::BluetoothPort => (false, "Bluetooth".into()),
                SerialPortType::PciPort => (false, "PCI".into()),
                SerialPortType::Unknown => (false, String::new()),
            };
            Candidate {
                name: p.port_name,
                usb,
                description,
            }
        })
        .collect())
}

fn looks_like_cable(c: &Candidate) -> bool {
    // On macOS every device appears twice; /dev/tty.* waits for carrier detect,
    // so only the /dev/cu.* twin is usable.
    if c.name.starts_with("/dev/tty.") {
        return false;
    }
    c.usb || c.name.contains("usbserial")
}

fn pick_port(candidates: &[Candidate]) -> Result<String> {
    let cables: Vec<_> = candidates.iter().filter(|c| looks_like_cable(c)).collect();
    match cables.as_slice() {
        [one] => Ok(one.name.clone()),
        [] => bail!("no USB serial cable found. Plug in the K+DCAN cable, then run `e36 ports`"),
        many => bail!(
            "several possible cables found, pick one with --port:\n{}",
            many.iter()
                .map(|c| format!("  {}", c.name))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    }
}

fn list_ports() -> Result<()> {
    let all = candidates()?;
    if all.is_empty() {
        println!("No serial ports found.");
    }
    for c in &all {
        let mark = if looks_like_cable(c) { "*" } else { " " };
        println!("{mark} {:<40} {}", c.name, c.description);
    }
    println!("\n* = likely cable (FTDI is usually 0403:6001, CH340 is 1a86:7523)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(name: &str, usb: bool) -> Candidate {
        Candidate {
            name: name.into(),
            usb,
            description: String::new(),
        }
    }

    #[test]
    fn picks_the_cu_twin_of_a_usb_cable() {
        let ports = [
            port("/dev/cu.Bluetooth-Incoming-Port", false),
            port("/dev/tty.usbserial-A50285BI", true),
            port("/dev/cu.usbserial-A50285BI", true),
        ];
        assert_eq!(pick_port(&ports).unwrap(), "/dev/cu.usbserial-A50285BI");
    }

    #[test]
    fn errors_when_no_cable() {
        let ports = [port("/dev/cu.Bluetooth-Incoming-Port", false)];
        assert!(pick_port(&ports).is_err());
    }

    #[test]
    fn errors_when_ambiguous() {
        let ports = [
            port("/dev/cu.usbserial-1", true),
            port("/dev/cu.usbserial-2", true),
        ];
        let msg = pick_port(&ports).unwrap_err().to_string();
        assert!(msg.contains("/dev/cu.usbserial-2"));
    }

    #[test]
    fn cli_parses() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
