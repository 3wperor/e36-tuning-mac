//! Knock & misfire tab: the ECU's global knock retard over time, a
//! per-cylinder misfire and knock-sensor status from the fault memory, and a
//! log of fault codes appearing or clearing while logging.

use std::collections::{BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use ds2::ms41::{self, FaultMemory, ENGINE_SPEED};
use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};

/// How much knock history the chart keeps.
const HISTORY: Duration = Duration::from_secs(60);
/// How many fault events the log keeps.
const MAX_EVENTS: usize = 100;

const OK: egui::Color32 = egui::Color32::from_rgb(60, 180, 90);
const BAD: egui::Color32 = egui::Color32::from_rgb(230, 80, 60);

pub struct FaultEvent {
    pub at: Instant,
    pub code: u8,
    pub appeared: bool,
    pub rpm: Option<f64>,
}

impl FaultEvent {
    /// One line for the log and the CSV, e.g. "new fault 240 Misfire, cylinder 3".
    pub fn text(&self) -> String {
        format!(
            "{} {} {}",
            if self.appeared {
                "new fault"
            } else {
                "fault cleared"
            },
            self.code,
            ms41::fault_codes::describe(self.code).unwrap_or("Unknown code")
        )
    }
}

#[derive(Default)]
pub struct KnockView {
    history: VecDeque<(Instant, Vec<u8>)>,
    pub unsupported: Option<String>,
    /// Codes in the most recent fault memory read; `None` before the first.
    active: Option<BTreeSet<u8>>,
    last_check: Option<Instant>,
    events: VecDeque<FaultEvent>,
}

impl KnockView {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The most recent knock reply, for recording.
    pub fn latest(&self) -> Option<&[u8]> {
        self.history.back().map(|(_, d)| d.as_slice())
    }

    pub fn on_knock(&mut self, at: Instant, data: Vec<u8>) {
        self.history.push_back((at, data));
        while self
            .history
            .front()
            .is_some_and(|(t, _)| at.duration_since(*t) > HISTORY)
        {
            self.history.pop_front();
        }
    }

    /// Takes a fresh fault memory read and returns the codes that appeared or
    /// cleared since the last one. The first read only sets the baseline.
    pub fn on_faults(&mut self, at: Instant, raw: &[u8], rpm: Option<f64>) -> Vec<&FaultEvent> {
        self.last_check = Some(at);
        let Some(faults) = (FaultMemory { raw: raw.to_vec() }).decode() else {
            return Vec::new();
        };
        let now: BTreeSet<u8> = faults.iter().map(|f| f.code).collect();
        let mut new = 0;
        if let Some(before) = &self.active {
            let appeared = now.difference(before).map(|&c| (c, true));
            let cleared = before.difference(&now).map(|&c| (c, false));
            let changes: Vec<_> = appeared.chain(cleared).collect();
            // Newest check first in the log, but in code order within a check.
            for (code, appeared) in changes.into_iter().rev() {
                self.events.push_front(FaultEvent {
                    at,
                    code,
                    appeared,
                    rpm,
                });
                new += 1;
            }
            self.events.truncate(MAX_EVENTS);
        }
        self.active = Some(now);
        self.events.iter().take(new).collect()
    }

    pub fn ui(&self, ui: &mut egui::Ui, live: &VecDeque<(Instant, Vec<u8>)>, start: Instant) {
        ui.heading("Knock retard (global)");
        self.knock_section(ui, live, start);
        ui.add_space(10.0);
        ui.heading("Misfire and knock sensors");
        self.cylinders(ui);
        ui.add_space(10.0);
        ui.heading("Fault events while logging");
        self.event_log(ui, start);
    }

    fn knock_section(
        &self,
        ui: &mut egui::Ui,
        live: &VecDeque<(Instant, Vec<u8>)>,
        start: Instant,
    ) {
        if let Some(why) = &self.unsupported {
            ui.colored_label(
                egui::Color32::YELLOW,
                format!("Not available on this ECU: {why}."),
            );
            return;
        }
        let Some(latest) = self.latest() else {
            ui.weak("Waiting for the first reading…");
            return;
        };
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{}", latest.first().copied().unwrap_or(0)))
                    .size(28.0)
                    .strong(),
            );
            ui.label("raw");
            ui.add_space(12.0);
            ui.monospace(format!("reply: {}", ds2::hex(latest)));
        });
        ui.label(
            egui::RichText::new(
                "Read with DS2 request 0B 93 (reported by the community, not yet verified on \
                 your car). Units are unknown until we see a real knock event, so this shows the \
                 raw first byte; more usually means more retard.",
            )
            .small()
            .weak(),
        );

        let secs = |t: &Instant| t.duration_since(start).as_secs_f64();
        let knock: PlotPoints = self
            .history
            .iter()
            .filter_map(|(t, d)| Some([secs(t), *d.first()? as f64]))
            .collect();
        let rpm: PlotPoints = live
            .iter()
            .filter_map(|(t, d)| Some([secs(t), ENGINE_SPEED.decode(d)?]))
            .collect();
        Plot::new("knock_raw")
            .height(110.0)
            .link_axis("knock", [true, false])
            .include_y(0.0)
            .y_axis_label("knock (raw)")
            .y_axis_min_width(48.0)
            .allow_drag(false)
            .allow_scroll(false)
            .show(ui, |plot| {
                plot.line(Line::new("Knock retard (raw)", knock).color(BAD).fill(0.0))
            });
        Plot::new("knock_rpm")
            .height(110.0)
            .link_axis("knock", [true, false])
            .x_axis_label("seconds")
            .y_axis_label("rpm")
            .y_axis_min_width(48.0)
            .allow_drag(false)
            .allow_scroll(false)
            .show(ui, |plot| plot.line(Line::new("Engine speed", rpm)));
    }

    fn cylinders(&self, ui: &mut egui::Ui) {
        let Some(active) = &self.active else {
            ui.weak("Waiting for the first fault memory check…");
            return;
        };
        ui.horizontal(|ui| {
            for cyl in 1..=6u8 {
                let misfire = active
                    .iter()
                    .any(|&c| ms41::misfire_cylinder(c) == Some(cyl));
                status_box(
                    ui,
                    &format!("Cyl {cyl}"),
                    if misfire { "Misfire" } else { "OK" },
                    !misfire,
                );
            }
            ui.add_space(16.0);
            for sensor in 1..=2u8 {
                let fault = active
                    .iter()
                    .any(|&c| ms41::knock_sensor(c) == Some(sensor));
                let cyls = if sensor == 1 { "1-3" } else { "4-6" };
                status_box(
                    ui,
                    &format!("Knock sensor {cyls}"),
                    if fault { "Fault" } else { "OK" },
                    !fault,
                );
            }
        });
        if let Some(t) = self.last_check {
            ui.label(
                egui::RichText::new(format!(
                    "From the fault memory, checked every 15 s (last check {} s ago). A Euro \
                     MS41.0 may not have misfire detection at all, in which case the cylinders \
                     stay OK whatever happens.",
                    t.elapsed().as_secs()
                ))
                .small()
                .weak(),
            );
        }
    }

    fn event_log(&self, ui: &mut egui::Ui, start: Instant) {
        if self.events.is_empty() {
            ui.weak("No fault codes have appeared or cleared since connecting.");
            return;
        }
        egui::Grid::new("fault_events")
            .striped(true)
            .show(ui, |ui| {
                ui.strong("Time");
                ui.strong("Event");
                ui.strong("Engine speed");
                ui.end_row();
                for e in &self.events {
                    ui.monospace(format!("{:.0} s", e.at.duration_since(start).as_secs_f64()));
                    let important = ms41::misfire_cylinder(e.code).is_some()
                        || ms41::knock_sensor(e.code).is_some();
                    if e.appeared && important {
                        ui.colored_label(BAD, e.text());
                    } else {
                        ui.label(e.text());
                    }
                    ui.monospace(e.rpm.map_or("—".into(), |r| format!("{r:.0} rpm")));
                    ui.end_row();
                }
            });
    }
}

fn status_box(ui: &mut egui::Ui, title: &str, state: &str, ok: bool) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(title).small());
            ui.label(
                egui::RichText::new(state)
                    .strong()
                    .color(if ok { OK } else { BAD }),
            );
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(codes: &[u8]) -> Vec<u8> {
        let mut raw = vec![codes.len() as u8];
        for &c in codes {
            raw.extend([c, 0, 0, 0, 0, 0, 0, 0]);
        }
        raw
    }

    #[test]
    fn first_read_is_only_a_baseline() {
        let mut k = KnockView::default();
        assert!(k.on_faults(Instant::now(), &memory(&[5]), None).is_empty());
        assert!(k.events.is_empty());
    }

    #[test]
    fn logs_codes_that_appear_and_clear() {
        let mut k = KnockView::default();
        let t = Instant::now();
        k.on_faults(t, &memory(&[5]), None);
        let new: Vec<String> = k
            .on_faults(t, &memory(&[5, 240]), Some(3100.0))
            .iter()
            .map(|e| e.text())
            .collect();
        assert_eq!(new, vec!["new fault 240 Misfire, cylinder 3"]);
        assert_eq!(k.events[0].rpm, Some(3100.0));

        let new: Vec<String> = k
            .on_faults(t, &memory(&[]), None)
            .iter()
            .map(|e| e.text())
            .collect();
        assert_eq!(
            new,
            vec![
                "fault cleared 5 Fuel injector, cylinder 2",
                "fault cleared 240 Misfire, cylinder 3"
            ]
        );
        assert_eq!(k.events.len(), 3);
    }

    #[test]
    fn unrecognised_fault_layout_changes_nothing() {
        let mut k = KnockView::default();
        let t = Instant::now();
        k.on_faults(t, &memory(&[5]), None);
        assert!(k.on_faults(t, &[3, 1, 2, 3, 4], None).is_empty());
        assert_eq!(k.active, Some(BTreeSet::from([5])));
    }

    #[test]
    fn keeps_a_minute_of_knock_history() {
        let mut k = KnockView::default();
        let t = Instant::now();
        k.on_knock(t, vec![1]);
        k.on_knock(t + Duration::from_secs(61), vec![2]);
        assert_eq!(k.history.len(), 1);
        assert_eq!(k.latest(), Some(&[2u8][..]));
    }
}
