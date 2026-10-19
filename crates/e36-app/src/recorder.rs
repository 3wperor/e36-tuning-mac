//! Writes live data to a CSV file that MegaLogViewer, Numbers or Excel can open.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ds2::ms41::LIVE_CHANNELS;

pub struct Recorder {
    pub path: PathBuf,
    file: BufWriter<File>,
    start: Instant,
}

impl Recorder {
    /// Starts a new log in `~/Documents/e36-logs/`.
    pub fn start() -> io::Result<Self> {
        let dir = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join("Documents")
            .join("e36-logs");
        fs::create_dir_all(&dir)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let path = dir.join(format!("log-{stamp}.csv"));
        let mut file = BufWriter::new(File::create(&path)?);
        write!(file, "time_s")?;
        for ch in LIVE_CHANNELS {
            write!(file, ",{} ({})", ch.name, ch.unit)?;
        }
        writeln!(file, ",raw_hex")?;
        Ok(Self {
            path,
            file,
            start: Instant::now(),
        })
    }

    pub fn write(&mut self, at: Instant, data: &[u8]) -> io::Result<()> {
        write!(
            self.file,
            "{:.3}",
            at.saturating_duration_since(self.start).as_secs_f64()
        )?;
        for ch in LIVE_CHANNELS {
            match ch.decode(data) {
                Some(v) => write!(self.file, ",{v:.2}")?,
                None => write!(self.file, ",")?,
            }
        }
        writeln!(self.file, ",{}", ds2::hex(data))
    }
}
