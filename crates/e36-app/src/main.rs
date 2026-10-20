//! Live data window for the E36 MS41 DME.

mod knock;
mod recorder;
mod vanos;
mod worker;

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ds2::ms41::{self, LIVE_CHANNELS};
use ds2::serial::available_ports;
use ds2::LinkStats;
use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};

use knock::KnockView;
use recorder::{Extras, Recorder, Selection};
use vanos::VanosView;
use worker::{Command, Event, Source, Worker};

/// How much history the chart keeps.
const HISTORY: Duration = Duration::from_secs(60);
/// How long a changed byte stays highlighted in the raw view.
const HIGHLIGHT: Duration = Duration::from_millis(1500);

const KEY_EXCLUDED: &str = "excluded_channels";
const KEY_RECORD_RAW: &str = "record_raw";
const KEY_RECORD_VANOS: &str = "record_vanos";
const KEY_RECORD_KNOCK: &str = "record_knock";
const KEY_RECORD_EVENTS: &str = "record_events";
const KEY_VANOS_SOURCE: &str = "vanos_source";

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("E36 MS41 Live")
            .with_inner_size([1000.0, 680.0]),
        ..Default::default()
    };
    eframe::run_native(
        "E36 MS41 Live",
        options,
        Box::new(|cc| {
            let mut app = App::new();
            if let Some(storage) = cc.storage {
                let excluded: Vec<String> =
                    eframe::get_value(storage, KEY_EXCLUDED).unwrap_or_default();
                let raw = eframe::get_value(storage, KEY_RECORD_RAW).unwrap_or(true);
                app.selection = Selection::from_excluded(&excluded, raw);
                app.selection.vanos = eframe::get_value(storage, KEY_RECORD_VANOS).unwrap_or(true);
                app.selection.knock = eframe::get_value(storage, KEY_RECORD_KNOCK).unwrap_or(true);
                app.selection.events =
                    eframe::get_value(storage, KEY_RECORD_EVENTS).unwrap_or(true);
                let source: Option<(usize, Option<u8>, bool)> =
                    eframe::get_value(storage, KEY_VANOS_SOURCE).flatten();
                app.vanos.source = source.map(|(offset, bit, invert)| ms41::Flag {
                    offset,
                    bit,
                    invert,
                });
            }
            // `e36-app --demo` opens straight into demo mode.
            // `--noisy` makes the demo ECU behave like a running engine on a
            // noisy line, to see the link recover.
            app.noisy = std::env::args().any(|a| a == "--noisy");
            if std::env::args().any(|a| a == "--demo") {
                app.demo = true;
                app.autoconnect = true;
            }
            Ok(Box::new(app))
        }),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Live,
    Raw,
    Faults,
    Vanos,
    Knock,
    Recording,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Conn {
    Disconnected,
    Connecting,
    Connected,
    /// Was connected, lost the link, trying to get it back.
    Reconnecting,
}

struct App {
    ports: Vec<String>,
    port: Option<String>,
    demo: bool,
    noisy: bool,
    autoconnect: bool,
    worker: Option<Worker>,
    conn: Conn,
    ident: Option<ms41::Ident>,
    link: Option<LinkStats>,
    status: Option<String>,
    live_note: Option<String>,

    connected_at: Instant,
    live: Option<Vec<u8>>,
    changed_at: Vec<Instant>,
    /// Raw live blocks from the last [`HISTORY`], for charts.
    history: VecDeque<(Instant, Vec<u8>)>,
    plot_channel: usize,

    faults: Option<Vec<u8>>,
    confirm_clear: bool,
    recorder: Option<Recorder>,
    selection: Selection,
    vanos: VanosView,
    knock: KnockView,
    /// Fault events not yet written to the recording.
    pending_events: Vec<String>,
    tab: Tab,
}

impl App {
    fn new() -> Self {
        let mut app = Self {
            ports: Vec::new(),
            port: None,
            demo: false,
            noisy: false,
            autoconnect: false,
            worker: None,
            conn: Conn::Disconnected,
            ident: None,
            link: None,
            status: None,
            live_note: None,
            connected_at: Instant::now(),
            live: None,
            changed_at: Vec::new(),
            history: VecDeque::new(),
            plot_channel: 0,
            faults: None,
            confirm_clear: false,
            recorder: None,
            selection: Selection::default(),
            vanos: VanosView::new(None),
            knock: KnockView::default(),
            pending_events: Vec::new(),
            tab: Tab::Live,
        };
        app.refresh_ports();
        app
    }

    fn refresh_ports(&mut self) {
        // macOS lists each device twice; /dev/tty.* waits for carrier detect,
        // so only the /dev/cu.* twin is usable.
        self.ports = available_ports()
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.port_name)
            .filter(|n| !n.starts_with("/dev/tty."))
            .collect();
        if self.port.as_ref().is_none_or(|p| !self.ports.contains(p)) {
            self.port = self
                .ports
                .iter()
                .find(|n| n.contains("usbserial"))
                .or(self.ports.first())
                .cloned();
        }
    }

    fn connect(&mut self, ctx: &egui::Context) {
        let source = if self.demo {
            Source::Demo { noisy: self.noisy }
        } else if let Some(p) = &self.port {
            Source::Cable(p.clone())
        } else {
            self.status = Some("No cable selected. Plug it in and press ⟳.".into());
            return;
        };
        let ctx = ctx.clone();
        self.worker = Some(Worker::spawn(source, move || ctx.request_repaint()));
        self.conn = Conn::Connecting;
        self.status = None;
        self.live_note = None;
        self.live = None;
        self.history.clear();
        self.vanos.reset();
        self.knock.reset();
        self.pending_events.clear();
        self.faults = None;
    }

    fn disconnect(&mut self) {
        self.worker = None; // dropping it tells the thread to stop
        self.conn = Conn::Disconnected;
        self.recorder = None;
        self.link = None;
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Connected(id) => {
                match self.conn {
                    // Keep the chart's time axis running across a reconnect.
                    Conn::Reconnecting => self.status = Some("Reconnected.".into()),
                    _ => self.connected_at = Instant::now(),
                }
                self.conn = Conn::Connected;
                self.ident = Some(id);
            }
            Event::Link(stats) => self.link = Some(stats),
            Event::Reconnecting(reason) => {
                self.conn = Conn::Reconnecting;
                self.status = Some(format!(
                    "Link lost, reconnecting… ({reason}). Recording carries on once it's back."
                ));
            }
            Event::Live(at, data) => self.on_live(at, data),
            Event::LiveUnsupported(msg) => self.live_note = Some(msg),
            Event::Knock(at, data) => self.knock.on_knock(at, data),
            Event::KnockUnsupported(msg) => self.knock.unsupported = Some(msg),
            Event::Faults(raw) => {
                let rpm = self
                    .live
                    .as_deref()
                    .and_then(|d| ms41::ENGINE_SPEED.decode(d));
                let new = self.knock.on_faults(Instant::now(), &raw, rpm);
                if let Some(e) = new.first() {
                    self.status = Some(format!("Fault watch: {}", e.text()));
                }
                let texts: Vec<String> = new.iter().map(|e| e.text()).collect();
                self.pending_events.extend(texts);
                self.faults = Some(raw);
            }
            Event::FaultsCleared => {
                self.status = Some("Fault memory cleared.".into());
                if let Some(w) = &self.worker {
                    w.send(Command::ReadFaults);
                }
            }
            Event::Error(msg) => self.status = Some(msg),
            Event::Disconnected(reason) => {
                self.disconnect();
                if reason.is_some() {
                    self.status = reason;
                }
            }
        }
    }

    fn on_live(&mut self, at: Instant, data: Vec<u8>) {
        if self.changed_at.len() != data.len() {
            self.changed_at = vec![at - HIGHLIGHT; data.len()];
        }
        if let Some(prev) = &self.live {
            for (i, (a, b)) in prev.iter().zip(&data).enumerate() {
                if a != b {
                    self.changed_at[i] = at;
                }
            }
        }
        if let Some(rec) = &mut self.recorder {
            let extras = Extras {
                knock: self.knock.latest(),
                events: &self.pending_events,
            };
            let result = rec.write(at, &data, &extras);
            self.pending_events.clear();
            if let Err(e) = result {
                self.status = Some(format!("recording stopped: {e}"));
                self.recorder = None;
            }
        }
        self.vanos.on_live(at, self.live.as_deref(), &data);
        self.history.push_back((at, data.clone()));
        while self
            .history
            .front()
            .is_some_and(|(t, _)| at.duration_since(*t) > HISTORY)
        {
            self.history.pop_front();
        }
        self.live = Some(data);
    }

    /// Live reads per second over the last few seconds.
    fn rate(&self) -> f64 {
        let Some((last, _)) = self.history.back() else {
            return 0.0;
        };
        let window = Duration::from_secs(3);
        let n = self
            .history
            .iter()
            .rev()
            .take_while(|(t, _)| last.duration_since(*t) <= window)
            .count();
        n as f64 / window.as_secs_f64()
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let idle = self.conn == Conn::Disconnected;
            ui.add_enabled_ui(idle && !self.demo, |ui| {
                egui::ComboBox::from_id_salt("port")
                    .width(260.0)
                    .selected_text(self.port.as_deref().unwrap_or("no cable found"))
                    .show_ui(ui, |ui| {
                        for p in &self.ports {
                            ui.selectable_value(&mut self.port, Some(p.clone()), p);
                        }
                    });
                if ui.button("⟳").on_hover_text("Refresh ports").clicked() {
                    self.refresh_ports();
                }
            });
            ui.add_enabled(idle, egui::Checkbox::new(&mut self.demo, "Demo (no car)"));

            match self.conn {
                Conn::Disconnected => {
                    if ui.button("Connect").clicked() {
                        self.connect(ui.ctx());
                    }
                }
                Conn::Connecting => {
                    ui.spinner();
                    ui.label("Connecting…");
                }
                Conn::Connected => {
                    if ui.button("Disconnect").clicked() {
                        self.disconnect();
                    }
                    let pn = self
                        .ident
                        .as_ref()
                        .and_then(|i| i.part_number_pretty())
                        .unwrap_or_else(|| "unknown".into());
                    ui.label(format!("DME {pn}  ·  {:.1} reads/s", self.rate()));
                    if let Some(link) = &self.link {
                        link_label(ui, link);
                    }
                }
                Conn::Reconnecting => {
                    if ui.button("Disconnect").clicked() {
                        self.disconnect();
                    }
                    ui.spinner();
                    ui.colored_label(egui::Color32::YELLOW, "Reconnecting…");
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let connected = self.conn == Conn::Connected;
                let recording = self.recorder.as_ref().map(|r| r.path.clone());
                if let Some(path) = recording {
                    if ui.button("Stop recording").clicked() {
                        self.recorder = None;
                        self.status = Some(format!("Saved {}", path.display()));
                    }
                } else if ui
                    .add_enabled(
                        connected && !self.selection.is_empty(self.vanos.source.is_some()),
                        egui::Button::new("Record CSV"),
                    )
                    .on_disabled_hover_text(if connected {
                        "Nothing selected in the Recording tab"
                    } else {
                        "Connect first"
                    })
                    .clicked()
                {
                    match Recorder::start(self.selection.clone(), self.vanos.source) {
                        Ok(r) => {
                            self.status = Some(format!("Recording to {}", r.path.display()));
                            self.recorder = Some(r);
                        }
                        Err(e) => self.status = Some(format!("can't record: {e}")),
                    }
                }
            });
        });
    }

    fn live_tab(&mut self, ui: &mut egui::Ui) {
        if let Some(note) = &self.live_note {
            ui.colored_label(
                egui::Color32::YELLOW,
                format!("Live data unavailable: {note}."),
            );
        }
        ui.label(
            egui::RichText::new(
                "Decoded values are UNVERIFIED for your ECU. Check them against the raw bytes before trusting them.",
            )
            .small()
            .weak(),
        );
        ui.add_space(6.0);

        let data = self.live.as_deref().unwrap_or(&[]);
        ui.columns(LIVE_CHANNELS.len(), |cols| {
            for (ch, ui) in LIVE_CHANNELS.iter().zip(cols) {
                ui.group(|ui| {
                    ui.label(ch.name);
                    let value = ch.decode(data);
                    ui.label(
                        egui::RichText::new(match value {
                            Some(v) => format!("{v:.0} {}", ch.unit),
                            None => "—".into(),
                        })
                        .size(30.0)
                        .strong(),
                    );
                    let frac = value.map_or(0.0, |v| ((v - ch.min) / (ch.max - ch.min)) as f32);
                    ui.add(egui::ProgressBar::new(frac.clamp(0.0, 1.0)));
                });
            }
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Chart:");
            for (i, ch) in LIVE_CHANNELS.iter().enumerate() {
                ui.selectable_value(&mut self.plot_channel, i, ch.name);
            }
        });
        let start = self.connected_at;
        let idx = self.plot_channel;
        let points: PlotPoints = self
            .history
            .iter()
            .filter_map(|(t, data)| {
                Some([
                    t.duration_since(start).as_secs_f64(),
                    LIVE_CHANNELS[idx].decode(data)?,
                ])
            })
            .collect();
        let ch = &LIVE_CHANNELS[idx];
        Plot::new("history")
            .x_axis_label("seconds")
            .y_axis_label(format!("{} ({})", ch.name, ch.unit))
            .allow_drag(false)
            .allow_scroll(false)
            .show(ui, |plot| plot.line(Line::new(ch.name, points)));
    }

    fn raw_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(
            "Every byte of the live data block. Bytes that just changed light up. Rev the engine \
             or let it warm up and watch which bytes move: that's how we confirm what each one means.",
        );
        ui.add_space(6.0);
        let Some(data) = &self.live else {
            ui.weak("No live data yet.");
            return;
        };
        let now = Instant::now();
        egui::Grid::new("raw").striped(true).show(ui, |ui| {
            for (i, b) in data.iter().enumerate() {
                let age = now.duration_since(self.changed_at[i]);
                let heat = 1.0 - (age.as_secs_f32() / HIGHLIGHT.as_secs_f32()).min(1.0);
                let text = egui::RichText::new(format!("{i:>2}: {b:02X} ({b:>3})")).monospace();
                let text = if heat > 0.0 {
                    text.background_color(egui::Color32::from_rgba_unmultiplied(
                        255,
                        140,
                        0,
                        (heat * 160.0) as u8,
                    ))
                } else {
                    text
                };
                ui.label(text);
                if i % 8 == 7 {
                    ui.end_row();
                }
            }
        });
    }

    fn recording_tab(&mut self, ui: &mut egui::Ui) {
        ui.label("Choose what goes into your CSV recordings. Your choice is remembered.");
        ui.add_space(6.0);
        let recording = self.recorder.is_some();
        if recording {
            ui.colored_label(
                egui::Color32::YELLOW,
                "Recording now. Stop recording to change this.",
            );
        }
        ui.add_enabled_ui(!recording, |ui| {
            ui.horizontal(|ui| {
                if ui.button("All").clicked() {
                    self.selection.set_all(true);
                }
                if ui.button("None").clicked() {
                    self.selection.set_all(false);
                }
            });
            ui.add_space(4.0);
            for (ch, on) in LIVE_CHANNELS.iter().zip(&mut self.selection.channels) {
                ui.checkbox(on, format!("{} ({})", ch.name, ch.unit));
            }
            let vanos_ready = self.vanos.source.is_some();
            ui.add_enabled(
                vanos_ready,
                egui::Checkbox::new(&mut self.selection.vanos, "VANOS (on/off)"),
            )
            .on_disabled_hover_text("Pick the VANOS bit in the VANOS tab first");
            ui.checkbox(&mut self.selection.knock, "Knock retard (raw)");
            ui.checkbox(
                &mut self.selection.events,
                "Fault events (codes appearing or clearing)",
            );
            ui.checkbox(&mut self.selection.raw, "Raw data block (all bytes)")
                .on_hover_text(
                    "Every byte the ECU sends. Keep this on for now: it's how we find \
                     engine load, O2 and VANOS in your recordings.",
                );
        });
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "All of these come from the same ECU reply, so switching some off keeps the \
                 file tidy but doesn't make logging faster.",
            )
            .small()
            .weak(),
        );
        ui.label(
            egui::RichText::new(format!("Files are saved to {}", Recorder::dir().display()))
                .small()
                .weak(),
        );
    }

    fn faults_tab(&mut self, ui: &mut egui::Ui) {
        let connected = self.conn == Conn::Connected;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(connected, egui::Button::new("Read faults"))
                .clicked()
            {
                if let Some(w) = &self.worker {
                    w.send(Command::ReadFaults);
                }
            }
            if ui
                .add_enabled(
                    connected && self.faults.is_some(),
                    egui::Button::new("Clear faults…"),
                )
                .on_disabled_hover_text("Read the faults first so they're on screen")
                .clicked()
            {
                self.confirm_clear = true;
            }
        });
        ui.add_space(6.0);
        match &self.faults {
            None => {
                ui.weak("Not read yet.");
            }
            Some(raw) => {
                let memory = ms41::FaultMemory { raw: raw.clone() };
                match memory.decode() {
                    Some(list) if list.is_empty() => {
                        ui.label(egui::RichText::new("No faults stored.").strong());
                    }
                    Some(list) => {
                        ui.label(
                            egui::RichText::new(format!("{} fault(s) stored", list.len())).strong(),
                        );
                        ui.add_space(4.0);
                        egui::Grid::new("faults").striped(true).show(ui, |ui| {
                            ui.strong("Code");
                            ui.strong("Description");
                            ui.strong("Record bytes");
                            ui.end_row();
                            for f in &list {
                                ui.monospace(f.code.to_string());
                                match f.description() {
                                    Some(d) => ui.label(d),
                                    None => ui.weak("Unknown code"),
                                };
                                ui.monospace(ds2::hex(&f.record));
                                ui.end_row();
                            }
                        });
                    }
                    None => {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "Fault reply layout not recognised; showing raw bytes only.",
                        );
                    }
                }
                ui.add_space(8.0);
                ui.weak(format!("Raw fault memory ({} bytes):", raw.len()));
                for chunk in raw.chunks(16) {
                    ui.monospace(ds2::hex(chunk));
                }
                ui.add_space(4.0);
                ui.weak(
                    "Descriptions come from community lists and the reply layout is unverified \
                     for your ECU. Check them against the raw bytes.",
                );
            }
        }

        if self.confirm_clear {
            egui::Window::new("Clear fault codes?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.label("This erases the stored fault codes in the DME.");
                    ui.horizontal(|ui| {
                        if ui.button("Erase").clicked() {
                            if let Some(w) = &self.worker {
                                w.send(Command::ClearFaults);
                            }
                            self.confirm_clear = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_clear = false;
                        }
                    });
                });
        }
    }
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, KEY_EXCLUDED, &self.selection.excluded());
        eframe::set_value(storage, KEY_RECORD_RAW, &self.selection.raw);
        eframe::set_value(storage, KEY_RECORD_VANOS, &self.selection.vanos);
        eframe::set_value(storage, KEY_RECORD_KNOCK, &self.selection.knock);
        eframe::set_value(storage, KEY_RECORD_EVENTS, &self.selection.events);
        let source = self.vanos.source.map(|f| (f.offset, f.bit, f.invert));
        eframe::set_value(storage, KEY_VANOS_SOURCE, &source);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if std::mem::take(&mut self.autoconnect) {
            self.connect(ui.ctx());
        }
        let events: Vec<Event> = self
            .worker
            .as_ref()
            .map(|w| w.events.try_iter().collect())
            .unwrap_or_default();
        for e in events {
            self.handle(e);
        }

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.label(self.status.as_deref().unwrap_or(match self.conn {
                Conn::Disconnected => {
                    "Not connected. Ignition to position II, then Connect (or tick Demo)."
                }
                Conn::Connecting => "Asking the DME who it is…",
                Conn::Connected => "Connected.",
                Conn::Reconnecting => "Reconnecting…",
            }));
        });
        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Live, "Live");
                ui.selectable_value(&mut self.tab, Tab::Raw, "Raw bytes");
                ui.selectable_value(&mut self.tab, Tab::Faults, "Faults");
                ui.selectable_value(&mut self.tab, Tab::Vanos, "VANOS");
                ui.selectable_value(&mut self.tab, Tab::Knock, "Knock & misfire");
                ui.selectable_value(&mut self.tab, Tab::Recording, "Recording");
            });
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                Tab::Live => self.live_tab(ui),
                Tab::Raw => self.raw_tab(ui),
                Tab::Faults => self.faults_tab(ui),
                Tab::Vanos => self.vanos.ui(
                    ui,
                    &self.history,
                    self.connected_at,
                    self.live.as_deref(),
                    self.demo,
                ),
                Tab::Knock => self.knock.ui(ui, &self.history, self.connected_at),
                Tab::Recording => self.recording_tab(ui),
            });
        });

        if matches!(self.conn, Conn::Connected | Conn::Reconnecting) {
            // Keep the byte highlights fading even between reads.
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    }
}

/// "link 97%" in green, yellow or red, with the details on hover.
fn link_label(ui: &mut egui::Ui, link: &LinkStats) {
    let quality = link.quality();
    let color = if quality >= 0.9 {
        egui::Color32::from_rgb(60, 180, 90)
    } else if quality >= 0.6 {
        egui::Color32::YELLOW
    } else {
        egui::Color32::from_rgb(230, 80, 60)
    };
    let mut details = format!(
        "Good replies: {}\nFailed: {}\nPause between requests: {} ms (adjusts itself)",
        link.good,
        link.failed,
        link.gap.as_millis()
    );
    if let Some(e) = &link.last_error {
        details += &format!("\nLast problem: {e}");
    }
    ui.colored_label(color, format!("·  link {:.0}%", quality * 100.0))
        .on_hover_text(details);
}
