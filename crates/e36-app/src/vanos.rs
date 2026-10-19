//! VANOS tab: on/off indicator, timeline against engine speed, a log of every
//! switch, and a bit finder for locating the VANOS flag in the live data.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ds2::ms41::{Flag, ENGINE_SPEED};
use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};

/// How many switches the log keeps.
const MAX_SWITCHES: usize = 50;
/// How long a bit stays highlighted in the finder after it flips.
const BIT_HIGHLIGHT: Duration = Duration::from_secs(2);

const ON: egui::Color32 = egui::Color32::from_rgb(60, 180, 90);
const FLIPPED: egui::Color32 = egui::Color32::from_rgb(230, 130, 0);

struct Switch {
    at: Instant,
    on: bool,
    rpm: Option<f64>,
}

pub struct VanosView {
    pub source: Option<Flag>,
    state: Option<bool>,
    switches: VecDeque<Switch>,
    bit_flipped_at: Vec<[Option<Instant>; 8]>,
}

impl VanosView {
    pub fn new(source: Option<Flag>) -> Self {
        Self {
            source,
            state: None,
            switches: VecDeque::new(),
            bit_flipped_at: Vec::new(),
        }
    }

    /// Forget everything seen so far (on a new connection).
    pub fn reset(&mut self) {
        self.state = None;
        self.switches.clear();
        self.bit_flipped_at.clear();
    }

    fn set_source(&mut self, source: Option<Flag>) {
        self.source = source;
        self.state = None;
        self.switches.clear();
    }

    pub fn on_live(&mut self, at: Instant, prev: Option<&[u8]>, data: &[u8]) {
        if self.bit_flipped_at.len() != data.len() {
            self.bit_flipped_at = vec![[None; 8]; data.len()];
        }
        if let Some(prev) = prev {
            for (i, (a, b)) in prev.iter().zip(data).enumerate() {
                let flipped = a ^ b;
                for bit in 0..8 {
                    if (flipped >> bit) & 1 == 1 {
                        self.bit_flipped_at[i][bit] = Some(at);
                    }
                }
            }
        }

        let Some(on) = self.source.and_then(|s| s.decode(data)) else {
            return;
        };
        if self.state.is_some_and(|s| s != on) {
            self.switches.push_front(Switch {
                at,
                on,
                rpm: ENGINE_SPEED.decode(data),
            });
            self.switches.truncate(MAX_SWITCHES);
        }
        self.state = Some(on);
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        history: &VecDeque<(Instant, Vec<u8>)>,
        start: Instant,
        live: Option<&[u8]>,
        demo: bool,
    ) {
        self.status_row(ui);
        ui.add_space(6.0);

        if let Some(source) = self.source {
            ui.columns(2, |cols| {
                self.timeline(&mut cols[0], history, start, source);
                self.switch_log(&mut cols[1], start);
            });
            ui.add_space(6.0);
        }

        egui::CollapsingHeader::new("Set up: find VANOS in the data")
            .default_open(self.source.is_none())
            .show(ui, |ui| self.finder(ui, live, demo));
    }

    fn status_row(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (text, color) = match (self.source, self.state) {
                (None, _) => (
                    "VANOS: not set up yet".to_string(),
                    ui.visuals().weak_text_color(),
                ),
                (Some(_), None) => (
                    "VANOS: waiting for data".into(),
                    ui.visuals().weak_text_color(),
                ),
                (Some(_), Some(true)) => ("VANOS ON".into(), ON),
                (Some(_), Some(false)) => ("VANOS OFF".into(), ui.visuals().text_color()),
            };
            ui.label(egui::RichText::new(text).size(28.0).strong().color(color));
            if let Some(last) = self.switches.front() {
                ui.add_space(16.0);
                ui.label(format!(
                    "{} switches · last: {} at {}",
                    self.switches.len(),
                    if last.on { "ON" } else { "OFF" },
                    rpm_text(last.rpm)
                ));
            }
        });
        if self.source.is_some() {
            ui.label(
                egui::RichText::new(
                    "Shows the bit you picked below. Check it matches what the engine is doing.",
                )
                .small()
                .weak(),
            );
        }
    }

    fn timeline(
        &self,
        ui: &mut egui::Ui,
        history: &VecDeque<(Instant, Vec<u8>)>,
        start: Instant,
        source: Flag,
    ) {
        let secs = |t: &Instant| t.duration_since(start).as_secs_f64();
        // Draw VANOS as steps: repeat the previous level at each switch.
        let mut steps = Vec::new();
        let mut prev: Option<f64> = None;
        for (t, data) in history {
            let Some(on) = source.decode(data) else {
                continue;
            };
            let level = if on { 1.0 } else { 0.0 };
            if let Some(p) = prev.filter(|p| *p != level) {
                steps.push([secs(t), p]);
            }
            steps.push([secs(t), level]);
            prev = Some(level);
        }
        let rpm: PlotPoints = history
            .iter()
            .filter_map(|(t, d)| Some([secs(t), ENGINE_SPEED.decode(d)?]))
            .collect();

        Plot::new("vanos_state")
            .height(90.0)
            .link_axis("vanos", [true, false])
            .include_y(-0.1)
            .include_y(1.1)
            .y_axis_label("VANOS")
            .y_axis_min_width(48.0)
            .allow_drag(false)
            .allow_scroll(false)
            .show(ui, |plot| {
                plot.line(
                    Line::new("VANOS", PlotPoints::from(steps))
                        .color(ON)
                        .fill(0.0),
                )
            });
        Plot::new("vanos_rpm")
            .height(170.0)
            .link_axis("vanos", [true, false])
            .x_axis_label("seconds")
            .y_axis_label("rpm")
            .y_axis_min_width(48.0)
            .allow_drag(false)
            .allow_scroll(false)
            .show(ui, |plot| plot.line(Line::new("Engine speed", rpm)));
    }

    fn switch_log(&self, ui: &mut egui::Ui, start: Instant) {
        ui.strong("Switch log");
        if self.switches.is_empty() {
            ui.weak("No switches yet. Rev the engine or drive.");
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("switches")
            .max_height(240.0)
            .show(ui, |ui| {
                egui::Grid::new("switch_log").striped(true).show(ui, |ui| {
                    ui.strong("Time");
                    ui.strong("Change");
                    ui.strong("Engine speed");
                    ui.end_row();
                    for s in &self.switches {
                        ui.monospace(format!("{:.1} s", s.at.duration_since(start).as_secs_f64()));
                        if s.on {
                            ui.colored_label(ON, "ON");
                        } else {
                            ui.label("OFF");
                        }
                        ui.monospace(rpm_text(s.rpm));
                        ui.end_row();
                    }
                });
            });
    }

    fn finder(&mut self, ui: &mut egui::Ui, live: Option<&[u8]>, demo: bool) {
        ui.label(
            "Every bit of the live data, as 0 or 1. Bits light up orange when they flip. Rev the \
             engine or drive so VANOS switches, watch for a bit that flips at the same moment \
             each time, and click it.",
        );
        if demo {
            ui.label(
                egui::RichText::new(format!(
                    "Demo mode: the pretend VANOS is byte {}, bit {}.",
                    ds2::SIM_VANOS.offset,
                    ds2::SIM_VANOS.bit.unwrap_or(0)
                ))
                .italics(),
            );
        }
        ui.add_space(4.0);

        ui.horizontal(|ui| match self.source {
            Some(s) => {
                ui.label(format!(
                    "Using byte {}, {}.",
                    s.offset,
                    s.bit.map_or("whole byte".into(), |b| format!("bit {b}"))
                ));
                let mut invert = s.invert;
                if ui
                    .checkbox(&mut invert, "Invert")
                    .on_hover_text("Tick if it shows ON when VANOS is actually off")
                    .changed()
                {
                    self.set_source(Some(Flag { invert, ..s }));
                }
                if ui.button("Clear").clicked() {
                    self.set_source(None);
                }
            }
            None => {
                ui.weak("Nothing picked yet.");
            }
        });
        ui.add_space(4.0);

        let Some(data) = live else {
            ui.weak("Connect to see the bits.");
            return;
        };
        let now = Instant::now();
        let invert = self.source.is_some_and(|s| s.invert);
        let mut picked = None;
        egui::Grid::new("bit_finder")
            .spacing([18.0, 4.0])
            .show(ui, |ui| {
                for (i, &byte) in data.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.monospace(format!("{i:>2}"));
                        for bit in (0..8u8).rev() {
                            let on = (byte >> bit) & 1 == 1;
                            let flipped = self
                                .bit_flipped_at
                                .get(i)
                                .and_then(|b| b[bit as usize])
                                .is_some_and(|t| now.duration_since(t) < BIT_HIGHLIGHT);
                            let selected = self
                                .source
                                .is_some_and(|s| s.offset == i && s.bit == Some(bit));
                            let fill = if flipped {
                                FLIPPED
                            } else {
                                ui.visuals().widgets.inactive.weak_bg_fill
                            };
                            let label = egui::RichText::new(if on { "1" } else { "0" })
                                .monospace()
                                .small();
                            let mut button = egui::Button::new(label)
                                .fill(fill)
                                .min_size(egui::vec2(16.0, 18.0));
                            if selected {
                                button = button.stroke(egui::Stroke::new(2.0, ON));
                            }
                            if ui
                                .add(button)
                                .on_hover_text(format!("Byte {i}, bit {bit}"))
                                .clicked()
                            {
                                picked = Some(Flag {
                                    offset: i,
                                    bit: Some(bit),
                                    invert,
                                });
                            }
                        }
                    });
                    if i % 4 == 3 {
                        ui.end_row();
                    }
                }
            });
        if picked.is_some() {
            self.set_source(picked);
        }
    }
}

fn rpm_text(rpm: Option<f64>) -> String {
    rpm.map_or("—".into(), |r| format!("{r:.0} rpm"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: Flag = Flag {
        offset: 2,
        bit: Some(0),
        invert: false,
    };

    fn block(rpm: f64, vanos: bool) -> Vec<u8> {
        let mut d = vec![0u8; 4];
        ENGINE_SPEED.encode(rpm, &mut d);
        d[2] = vanos as u8;
        d
    }

    #[test]
    fn logs_switches_with_engine_speed() {
        let mut v = VanosView::new(Some(SRC));
        let t = Instant::now();
        let a = block(900.0, false);
        let b = block(1700.0, true);
        let c = block(4300.0, false);
        v.on_live(t, None, &a);
        v.on_live(t, Some(&a), &b);
        v.on_live(t, Some(&b), &b);
        v.on_live(t, Some(&b), &c);
        let log: Vec<_> = v.switches.iter().map(|s| (s.on, s.rpm)).collect();
        assert_eq!(log, vec![(false, Some(4300.0)), (true, Some(1700.0))]);
        assert_eq!(v.state, Some(false));
    }

    #[test]
    fn first_reading_is_not_a_switch() {
        let mut v = VanosView::new(Some(SRC));
        v.on_live(Instant::now(), None, &block(2000.0, true));
        assert!(v.switches.is_empty());
        assert_eq!(v.state, Some(true));
    }

    #[test]
    fn tracks_flipped_bits_without_a_source() {
        let mut v = VanosView::new(None);
        let t = Instant::now();
        v.on_live(t, None, &[0b0000_0000]);
        v.on_live(t, Some(&[0b0000_0000]), &[0b0100_0001]);
        assert!(v.bit_flipped_at[0][0].is_some());
        assert!(v.bit_flipped_at[0][6].is_some());
        assert!(v.bit_flipped_at[0][1].is_none());
        assert!(v.switches.is_empty());
    }

    #[test]
    fn changing_source_clears_the_log() {
        let mut v = VanosView::new(Some(SRC));
        let t = Instant::now();
        let a = block(900.0, false);
        let b = block(1700.0, true);
        v.on_live(t, None, &a);
        v.on_live(t, Some(&a), &b);
        assert_eq!(v.switches.len(), 1);
        v.set_source(Some(Flag {
            invert: true,
            ..SRC
        }));
        assert!(v.switches.is_empty());
        assert_eq!(v.state, None);
    }
}
