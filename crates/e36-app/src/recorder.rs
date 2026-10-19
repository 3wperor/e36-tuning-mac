//! Writes live data to a CSV file that MegaLogViewer, Numbers or Excel can open.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ds2::ms41::{Flag, LIVE_CHANNELS};

/// Which values go into a recording, one flag per entry in [`LIVE_CHANNELS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub channels: Vec<bool>,
    /// The whole raw data block, needed to find values not decoded yet.
    pub raw: bool,
    /// VANOS on/off, once its bit has been picked.
    pub vanos: bool,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            channels: vec![true; LIVE_CHANNELS.len()],
            raw: true,
            vanos: true,
        }
    }
}

impl Selection {
    /// Rebuilds a selection from the channel names that were switched off.
    /// Stored by name so channels added in later versions start switched on.
    pub fn from_excluded(excluded: &[String], raw: bool) -> Self {
        Self {
            channels: LIVE_CHANNELS
                .iter()
                .map(|ch| !excluded.iter().any(|n| n == ch.name))
                .collect(),
            raw,
            vanos: true,
        }
    }

    pub fn excluded(&self) -> Vec<String> {
        LIVE_CHANNELS
            .iter()
            .zip(&self.channels)
            .filter(|(_, on)| !**on)
            .map(|(ch, _)| ch.name.to_string())
            .collect()
    }

    /// True when a recording would have no columns besides time.
    pub fn is_empty(&self, vanos_available: bool) -> bool {
        !self.raw && !(self.vanos && vanos_available) && !self.channels.contains(&true)
    }

    pub fn set_all(&mut self, on: bool) {
        self.channels.fill(on);
        self.raw = on;
        self.vanos = on;
    }

    fn header(&self, vanos: Option<Flag>) -> String {
        let mut line = String::from("time_s");
        for (ch, _) in self.selected() {
            line += &format!(",{} ({})", ch.name, ch.unit);
        }
        if vanos.is_some() {
            line += ",VANOS (on/off)";
        }
        if self.raw {
            line += ",raw_hex";
        }
        line
    }

    fn row(&self, seconds: f64, data: &[u8], vanos: Option<Flag>) -> String {
        let mut line = format!("{seconds:.3}");
        for (ch, _) in self.selected() {
            line += &match ch.decode(data) {
                Some(v) => format!(",{v:.2}"),
                None => ",".into(),
            };
        }
        if let Some(flag) = vanos {
            line += match flag.decode(data) {
                Some(true) => ",1",
                Some(false) => ",0",
                None => ",",
            };
        }
        if self.raw {
            line += &format!(",{}", ds2::hex(data));
        }
        line
    }

    fn selected(&self) -> impl Iterator<Item = (&'static ds2::ms41::Channel, &bool)> {
        LIVE_CHANNELS
            .iter()
            .zip(&self.channels)
            .filter(|(_, on)| **on)
    }
}

pub struct Recorder {
    pub path: PathBuf,
    file: BufWriter<File>,
    start: Instant,
    selection: Selection,
    /// The VANOS flag, if it's being recorded.
    vanos: Option<Flag>,
}

impl Recorder {
    /// Folder the logs go into: `~/Documents/e36-logs/`.
    pub fn dir() -> PathBuf {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join("Documents")
            .join("e36-logs")
    }

    pub fn start(selection: Selection, vanos: Option<Flag>) -> io::Result<Self> {
        let vanos = vanos.filter(|_| selection.vanos);
        let dir = Self::dir();
        fs::create_dir_all(&dir)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let path = dir.join(format!("log-{stamp}.csv"));
        let mut file = BufWriter::new(File::create(&path)?);
        writeln!(file, "{}", selection.header(vanos))?;
        Ok(Self {
            path,
            file,
            start: Instant::now(),
            selection,
            vanos,
        })
    }

    pub fn write(&mut self, at: Instant, data: &[u8]) -> io::Result<()> {
        let seconds = at.saturating_duration_since(self.start).as_secs_f64();
        writeln!(
            self.file,
            "{}",
            self.selection.row(seconds, data, self.vanos)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block() -> Vec<u8> {
        let mut data = vec![0u8; 16];
        LIVE_CHANNELS[0].encode(1000.0, &mut data);
        data
    }

    #[test]
    fn everything_selected_by_default() {
        let sel = Selection::default();
        assert_eq!(sel.header(None).split(',').count(), LIVE_CHANNELS.len() + 2);
        assert!(sel.row(1.0, &block(), None).starts_with("1.000,1000.00,"));
    }

    #[test]
    fn unselected_channels_and_raw_are_left_out() {
        let mut sel = Selection::default();
        sel.set_all(false);
        sel.channels[0] = true;
        assert_eq!(sel.header(None), "time_s,Engine speed (rpm)");
        assert_eq!(sel.row(0.5, &block(), None), "0.500,1000.00");
    }

    #[test]
    fn raw_only() {
        let mut sel = Selection::default();
        sel.channels.fill(false);
        assert_eq!(sel.header(None), "time_s,raw_hex");
        assert_eq!(sel.row(0.0, &[0x03, 0xE8], None), "0.000,03 E8");
    }

    #[test]
    fn excluded_names_roundtrip() {
        let mut sel = Selection::default();
        sel.channels[1] = false;
        let restored = Selection::from_excluded(&sel.excluded(), sel.raw);
        assert_eq!(restored, sel);
        assert_eq!(sel.excluded(), vec![LIVE_CHANNELS[1].name.to_string()]);
    }

    #[test]
    fn empty_when_nothing_selected() {
        let mut sel = Selection::default();
        sel.set_all(false);
        assert!(sel.is_empty(true));
        sel.raw = true;
        assert!(!sel.is_empty(true));
    }

    #[test]
    fn vanos_counts_only_once_it_is_set_up() {
        let mut sel = Selection::default();
        sel.set_all(false);
        sel.vanos = true;
        assert!(sel.is_empty(false));
        assert!(!sel.is_empty(true));
    }

    #[test]
    fn vanos_column_written_as_one_or_zero() {
        let mut sel = Selection::default();
        sel.set_all(false);
        let flag = Flag {
            offset: 1,
            bit: Some(0),
            invert: false,
        };
        assert_eq!(sel.header(Some(flag)), "time_s,VANOS (on/off)");
        assert_eq!(sel.row(0.0, &[0, 1], Some(flag)), "0.000,1");
        assert_eq!(sel.row(0.0, &[0, 0], Some(flag)), "0.000,0");
    }
}
