//! Tuning practice tab: a simulated engine, editable fuel and ignition maps,
//! a dyno, and scenarios with goals. Nothing here talks to the car.

use std::collections::BTreeSet;

use eframe::egui;
use egui_plot::{HLine, Legend, Line, LineStyle, Plot, PlotPoints};
use tunesim::{
    assess, cam_comparison, dyno_pull, fuel_correction, intake_tuned_rpm, snapshots, Check, Engine,
    Flare, Kind, Sample, Scenario, Table, Tune, CAM_RANGE, HEAD_PORT_MM, LEAN_WARNING, LOAD_AXIS,
    LUMPY_CAM_DURATION, POWER_AFR, RPM_AXIS, RUNNER_RANGE_MM, STOCK_CAM_DURATION, TRUMPET_RANGE_MM,
    WOT_BAND,
};

const PASS: egui::Color32 = egui::Color32::from_rgb(60, 180, 90);
const FAIL: egui::Color32 = egui::Color32::from_rgb(230, 80, 60);
const WARN: egui::Color32 = egui::Color32::from_rgb(230, 170, 40);
const PREVIOUS: egui::Color32 = egui::Color32::from_gray(120);
const VANOS_ON: egui::Color32 = egui::Color32::from_rgb(120, 200, 130);
const VANOS_OFF: egui::Color32 = egui::Color32::from_gray(150);
/// Undo steps kept.
const MAX_UNDO: usize = 100;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Map {
    Fuel,
    Ignition,
    Vanos,
}

enum Click {
    Cell(usize, usize),
    Row(usize),
    Col(usize),
}

pub struct Practice {
    scenarios: Vec<Scenario>,
    current: usize,
    engine: Engine,
    tune: Tune,
    undo: Vec<Tune>,
    map: Map,
    selected: BTreeSet<(usize, usize)>,
    pull: Option<Vec<Sample>>,
    /// Pulls with VANOS forced on and forced off.
    compare: Option<(Vec<Sample>, Vec<Sample>)>,
    previous: Option<Vec<Sample>>,
    checks: Option<Vec<Check>>,
    hints_shown: usize,
    answer: Option<usize>,
    submitted: bool,
    leak_fixed: bool,
    scale_percent: f64,
}

impl Practice {
    pub fn new() -> Self {
        let mut p = Self {
            scenarios: tunesim::scenarios(),
            current: 0,
            engine: Engine::default(),
            tune: tunesim::stock_tune(),
            undo: Vec::new(),
            map: Map::Fuel,
            selected: BTreeSet::new(),
            pull: None,
            previous: None,
            compare: None,
            checks: None,
            hints_shown: 0,
            answer: None,
            submitted: false,
            leak_fixed: false,
            scale_percent: 0.0,
        };
        p.load(0);
        p
    }

    fn load(&mut self, index: usize) {
        let s = &self.scenarios[index];
        self.current = index;
        self.engine = s.engine;
        self.tune = s.tune.clone();
        self.undo.clear();
        self.selected.clear();
        self.pull = None;
        self.previous = None;
        self.compare = None;
        self.checks = None;
        self.hints_shown = 0;
        self.answer = None;
        self.submitted = false;
        self.leak_fixed = false;
        self.map = Map::Fuel;
    }

    fn scenario(&self) -> &Scenario {
        &self.scenarios[self.current]
    }

    fn engine(&self) -> Engine {
        if self.leak_fixed {
            Engine {
                vacuum_leak: 0.0,
                ..self.engine
            }
        } else {
            self.engine
        }
    }

    fn table(&self) -> &Table {
        match self.map {
            Map::Fuel => &self.tune.fuel,
            Map::Ignition => &self.tune.ignition,
            Map::Vanos => &self.tune.vanos,
        }
    }

    /// Changes the current map, keeping an undo step.
    fn edit(&mut self, change: impl FnOnce(&mut Table, &BTreeSet<(usize, usize)>)) {
        self.undo.push(self.tune.clone());
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        let table = match self.map {
            Map::Fuel => &mut self.tune.fuel,
            Map::Ignition => &mut self.tune.ignition,
            Map::Vanos => &mut self.tune.vanos,
        };
        change(table, &self.selected);
    }

    fn run_pull(&mut self) {
        let engine = self.engine();
        self.previous = self.pull.take();
        self.pull = Some(dyno_pull(&engine, &self.tune));
        self.checks = (self.scenario().kind == Kind::Tuning)
            .then(|| assess(self.scenario(), &engine, &self.tune));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.colored_label(
            WARN,
            "Practice only: a simulated engine. Nothing here is sent to the car.",
        );
        ui.horizontal(|ui| {
            let mut pick = self.current;
            egui::ComboBox::from_id_salt("scenario")
                .width(260.0)
                .selected_text(self.scenario().title)
                .show_ui(ui, |ui| {
                    for (i, s) in self.scenarios.iter().enumerate() {
                        ui.selectable_value(&mut pick, i, s.title);
                    }
                });
            if pick != self.current {
                self.load(pick);
            }
            if ui.button("Restart scenario").clicked() {
                self.load(self.current);
            }
        });
        ui.add_space(6.0);
        self.brief(ui);
        ui.add_space(8.0);
        match self.scenario().kind {
            Kind::Diagnosis => self.diagnosis(ui),
            Kind::Tuning | Kind::Sandbox => self.tuning(ui),
        }
    }

    fn brief(&mut self, ui: &mut egui::Ui) {
        let s = &self.scenarios[self.current];
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(s.story).italics());
            ui.label(egui::RichText::new(format!("Task: {}", s.task)).strong());
            for hint in &s.hints[..self.hints_shown] {
                ui.label(format!("Hint: {hint}"));
            }
            if self.hints_shown < s.hints.len()
                && ui
                    .small_button(format!(
                        "Show a hint ({} of {})",
                        self.hints_shown + 1,
                        s.hints.len()
                    ))
                    .clicked()
            {
                self.hints_shown += 1;
            }
            if let Some(checks) = &self.checks {
                ui.separator();
                if checks.iter().all(|c| c.ok) {
                    ui.colored_label(PASS, "All goals met. Scenario complete!");
                }
                for c in checks {
                    ui.horizontal(|ui| {
                        if c.ok {
                            ui.colored_label(PASS, "PASS");
                        } else {
                            ui.colored_label(FAIL, "FAIL");
                        }
                        ui.label(&c.text);
                    });
                }
            }
        });
    }

    fn trims(&self, ui: &mut egui::Ui) {
        let snaps = snapshots(&self.engine(), &self.tune);
        egui::Grid::new("practice_trims")
            .striped(true)
            .show(ui, |ui| {
                for h in [
                    "Condition",
                    "RPM",
                    "Load",
                    "AFR",
                    "Fuel trim",
                    "VANOS",
                    "Mode",
                ] {
                    ui.strong(h);
                }
                ui.end_row();
                for (name, s) in snaps {
                    ui.label(name);
                    ui.monospace(format!("{:.0}", s.rpm));
                    ui.monospace(format!("{:.0}%", s.load));
                    ui.monospace(format!("{:.1}", s.afr));
                    let trim = format!("{:+.0}%", s.trim * 100.0);
                    if s.trim.abs() >= 0.24 {
                        ui.colored_label(FAIL, format!("{trim} (maxed out)"));
                    } else if s.trim.abs() > 0.05 {
                        ui.colored_label(WARN, trim);
                    } else {
                        ui.monospace(trim);
                    }
                    if s.rough_idle {
                        ui.colored_label(
                            FAIL,
                            if s.vanos {
                                "ON (lumpy idle)"
                            } else {
                                "off (lumpy idle: big cam)"
                            },
                        );
                    } else {
                        ui.label(if s.vanos { "ON" } else { "off" });
                    }
                    ui.label(if s.closed_loop {
                        "closed loop (O2 sensors)"
                    } else {
                        "open loop (map only)"
                    });
                    ui.end_row();
                }
            });
    }

    fn diagnosis(&mut self, ui: &mut egui::Ui) {
        ui.heading("What the ECU is doing");
        self.trims(ui);
        ui.add_space(8.0);

        let Some(q) = self.scenarios[self.current].question.as_ref() else {
            return;
        };
        ui.heading(q.prompt);
        for (i, (text, _)) in q.answers.iter().enumerate() {
            ui.radio_value(&mut self.answer, Some(i), *text);
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.answer.is_some() && !self.submitted,
                    egui::Button::new("Check answer"),
                )
                .clicked()
            {
                self.submitted = true;
            }
            if self.submitted && ui.button("Try another answer").clicked() {
                self.submitted = false;
            }
        });
        if let (true, Some(a)) = (self.submitted, self.answer) {
            let (_, why) = q.answers[a];
            let right = a == q.correct;
            ui.colored_label(
                if right { PASS } else { WARN },
                if right { "Correct." } else { "Not quite." },
            );
            ui.label(why);
            if right {
                ui.add_space(6.0);
                ui.checkbox(
                    &mut self.leak_fixed,
                    "Fix the leak (simulate the repair) and watch the trims",
                );
            }
        }
    }

    fn tuning(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Everyday driving: AFR and fuel trims")
            .default_open(true)
            .show(ui, |ui| self.trims(ui));
        ui.add_space(6.0);
        if self.scenario().hardware.any() {
            self.hardware(ui);
        }

        ui.horizontal(|ui| {
            let before = self.map;
            ui.selectable_value(&mut self.map, Map::Fuel, "Fuel map (injection time, ms)");
            ui.selectable_value(
                &mut self.map,
                Map::Ignition,
                "Ignition map (degrees advance)",
            );
            ui.selectable_value(&mut self.map, Map::Vanos, "VANOS map (on/off)");
            if self.map != before {
                self.selected.clear();
            }
        });
        self.tools(ui);
        self.grid(ui);
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new(
                    egui::RichText::new("Run dyno pull").strong(),
                ))
                .clicked()
            {
                self.run_pull();
            }
            if let Some(pull) = &self.pull {
                let peak = peak_by(pull, |s| s.power_bhp);
                let torque = peak_by(pull, |s| s.torque_nm);
                let mut text = format!(
                    "Peak {:.0} bhp at {:.0} rpm · {:.0} Nm at {:.0} rpm",
                    peak.power_bhp, peak.rpm, torque.torque_nm, torque.rpm
                );
                if let Some(prev) = &self.previous {
                    text += &format!(
                        " (last run {:.0} bhp)",
                        peak_by(prev, |s| s.power_bhp).power_bhp
                    );
                }
                ui.label(text);
            }
        });
        if ui
            .button("Compare cam positions")
            .on_hover_text(
                "Runs one pull with VANOS always on and one always off, to show where each cam \
                 position makes more torque.",
            )
            .clicked()
        {
            self.compare = Some(cam_comparison(&self.engine(), &self.tune));
        }
        if self.pull.is_some() {
            self.charts(ui);
        }
        if let Some((on, off)) = &self.compare {
            cam_chart(ui, on, off);
        }
    }

    /// Engine parts the scenario lets you change: intake, runners, trumpets, cams.
    fn hardware(&mut self, ui: &mut egui::Ui) {
        let hw = self.scenario().hardware;
        let mut e = self.engine;
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.strong("Engine hardware");
            if hw.intake_choice {
                ui.horizontal(|ui| {
                    ui.label("Intake");
                    let mut itb = e.itb_runner_mm.is_some();
                    ui.radio_value(&mut itb, false, "Standard (air flow meter)");
                    ui.radio_value(&mut itb, true, "ITBs (fuel from throttle position)");
                    match (itb, e.itb_runner_mm) {
                        (true, None) => e.itb_runner_mm = Some(200.0),
                        (false, Some(_)) => e.itb_runner_mm = None,
                        _ => {}
                    }
                });
            }
            if let Some(mut runner) = e.itb_runner_mm {
                ui.horizontal(|ui| {
                    ui.label("Runner length");
                    if hw.runner {
                        let (lo, hi) = RUNNER_RANGE_MM;
                        if ui
                            .add(
                                egui::Slider::new(&mut runner, lo..=hi)
                                    .step_by(5.0)
                                    .suffix(" mm"),
                            )
                            .changed()
                        {
                            e.itb_runner_mm = Some(runner);
                        }
                    } else {
                        ui.label(format!("{runner:.0} mm (fixed)"));
                    }
                });
                if hw.trumpets {
                    ui.horizontal(|ui| {
                        ui.label("Trumpet length");
                        let (lo, hi) = TRUMPET_RANGE_MM;
                        ui.add(
                            egui::Slider::new(&mut e.trumpet_mm, lo..=hi)
                                .step_by(5.0)
                                .suffix(" mm"),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Bellmouth");
                        for f in Flare::ALL {
                            ui.radio_value(&mut e.flare, f, f.label());
                        }
                    });
                }
                if let Some(rpm) = intake_tuned_rpm(&e) {
                    ui.label(
                        egui::RichText::new(format!("Intake tuned for about {rpm:.0} rpm"))
                            .strong(),
                    );
                }
                ui.label(
                    egui::RichText::new(format!(
                        "Tuned length = runner + trumpet + the {HEAD_PORT_MM:.0} mm port in the \
                         head. Shorter tunes higher up the revs; longer gives more low-down torque."
                    ))
                    .small()
                    .weak(),
                );
            }
            if hw.cams {
                ui.horizontal(|ui| {
                    ui.label("Cam duration");
                    let (lo, hi) = CAM_RANGE;
                    ui.add(
                        egui::Slider::new(&mut e.cam_duration, lo..=hi)
                            .step_by(1.0)
                            .suffix("°"),
                    );
                    if e.cam_duration > LUMPY_CAM_DURATION {
                        ui.colored_label(WARN, "lumpy idle");
                    }
                });
                ui.label(
                    egui::RichText::new(format!(
                        "Baseline {STOCK_CAM_DURATION:.0}° (illustrative, not an exact M52 spec). \
                         Longer duration: more top-end power, less low-down torque, and above \
                         about {LUMPY_CAM_DURATION:.0}° a lumpy idle."
                    ))
                    .small()
                    .weak(),
                );
            }
        });
        if e != self.engine {
            self.engine = e;
            // Results were for the old hardware.
            self.checks = None;
            self.compare = None;
        }
        ui.add_space(6.0);
    }

    fn tools(&mut self, ui: &mut egui::Ui) {
        let any = !self.selected.is_empty();
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("{} cells selected", self.selected.len()));
            let steps: &[(&str, f64)] = match self.map {
                Map::Fuel => &[("-5%", -5.0), ("-1%", -1.0), ("+1%", 1.0), ("+5%", 5.0)],
                Map::Ignition => &[("-2°", -2.0), ("-0.5°", -0.5), ("+0.5°", 0.5), ("+2°", 2.0)],
                Map::Vanos => &[],
            };
            if self.map == Map::Vanos {
                for (label, value) in [("Turn ON", 1.0), ("Turn OFF", 0.0)] {
                    if ui.add_enabled(any, egui::Button::new(label)).clicked() {
                        self.edit(|t, sel| {
                            for &(r, c) in sel {
                                t.set(r, c, value);
                            }
                        });
                    }
                }
            }
            for &(label, step) in steps {
                if ui.add_enabled(any, egui::Button::new(label)).clicked() {
                    let fuel = self.map == Map::Fuel;
                    self.edit(|t, sel| {
                        for &(r, c) in sel {
                            let v = t.get(r, c);
                            t.set(
                                r,
                                c,
                                if fuel {
                                    v * (1.0 + step / 100.0)
                                } else {
                                    v + step
                                },
                            );
                        }
                    });
                }
            }
            if self.map == Map::Fuel {
                let can_correct = any && self.pull.is_some();
                if ui
                    .add_enabled(can_correct, egui::Button::new("Correct from last pull"))
                    .on_hover_text(format!(
                        "For each selected cell: new fuel = old fuel × measured AFR / {POWER_AFR} \
                         target, using the full-throttle reading at that cell's RPM."
                    ))
                    .on_disabled_hover_text("Run a dyno pull and select some cells first")
                    .clicked()
                {
                    let pull = self.pull.clone().unwrap_or_default();
                    self.edit(|t, sel| {
                        for &(r, c) in sel {
                            if let Some(s) = pull.iter().find(|s| s.rpm == RPM_AXIS[c]) {
                                t.set(r, c, t.get(r, c) * fuel_correction(s.afr, POWER_AFR));
                            }
                        }
                    });
                }
            }
            if ui.button("Select all").clicked() {
                self.selected = all_cells().collect();
            }
            if ui
                .add_enabled(any, egui::Button::new("Clear selection"))
                .clicked()
            {
                self.selected.clear();
            }
        });
        ui.horizontal_wrapped(|ui| {
            if self.map != Map::Vanos {
                let unit = if self.map == Map::Fuel { "%" } else { "°" };
                ui.label(if self.map == Map::Fuel {
                    "Scale whole table by"
                } else {
                    "Shift whole table by"
                });
                ui.add(
                    egui::DragValue::new(&mut self.scale_percent)
                        .speed(0.1)
                        .range(-50.0..=50.0)
                        .suffix(unit),
                );
                if ui.button("Apply").clicked() {
                    let amount = self.scale_percent;
                    let fuel = self.map == Map::Fuel;
                    self.edit(|t, _| {
                        t.map(|v| {
                            if fuel {
                                v * (1.0 + amount / 100.0)
                            } else {
                                v + amount
                            }
                        })
                    });
                }
                ui.separator();
            }
            if ui
                .add_enabled(!self.undo.is_empty(), egui::Button::new("Undo"))
                .clicked()
            {
                if let Some(t) = self.undo.pop() {
                    self.tune = t;
                }
            }
            if ui.button("Reset this map").clicked() {
                let start = self.scenarios[self.current].tune.clone();
                let map = self.map;
                self.edit(|t, _| {
                    *t = match map {
                        Map::Fuel => start.fuel,
                        Map::Ignition => start.ignition,
                        Map::Vanos => start.vanos,
                    }
                });
            }
        });
    }

    fn grid(&mut self, ui: &mut egui::Ui) {
        let table = self.table();
        let (lo, hi) = table.min_max();
        let map = self.map;
        let fuel = map == Map::Fuel;
        let mut click = None;
        egui::ScrollArea::horizontal()
            .id_salt("practice_grid")
            .show(ui, |ui| {
                egui::Grid::new("practice_map")
                    .spacing([2.0, 2.0])
                    .show(ui, |ui| {
                        ui.label("load \\ rpm");
                        for (c, rpm) in RPM_AXIS.iter().enumerate() {
                            if ui.small_button(format!("{rpm:.0}")).clicked() {
                                click = Some(Click::Col(c));
                            }
                        }
                        ui.end_row();
                        for (r, load) in LOAD_AXIS.iter().enumerate() {
                            if ui.small_button(format!("{load:.0}%")).clicked() {
                                click = Some(Click::Row(r));
                            }
                            for c in 0..Table::COLS {
                                let v = table.get(r, c);
                                let (text, fill) = match map {
                                    Map::Fuel => (format!("{v:.2}"), heat(v, lo, hi)),
                                    Map::Ignition => (format!("{v:.1}"), heat(v, lo, hi)),
                                    Map::Vanos if v >= 0.5 => ("ON".into(), VANOS_ON),
                                    Map::Vanos => ("off".into(), VANOS_OFF),
                                };
                                let mut button = egui::Button::new(
                                    egui::RichText::new(text)
                                        .monospace()
                                        .size(11.0)
                                        .color(egui::Color32::BLACK),
                                )
                                .fill(fill)
                                .min_size(egui::vec2(46.0, 20.0));
                                if self.selected.contains(&(r, c)) {
                                    button =
                                        button.stroke(egui::Stroke::new(2.0, egui::Color32::WHITE));
                                }
                                if ui.add(button).clicked() {
                                    click = Some(Click::Cell(r, c));
                                }
                            }
                            ui.end_row();
                        }
                        if let (Map::Vanos, Some((on, off))) = (map, &self.compare) {
                            ui.label("Better at full throttle");
                            for &rpm in &RPM_AXIS {
                                let at = |p: &[Sample]| {
                                    p.iter().find(|s| s.rpm == rpm).map(|s| s.torque_nm)
                                };
                                match (at(on), at(off)) {
                                    (Some(a), Some(b)) if a > b => {
                                        ui.colored_label(PASS, "ON");
                                    }
                                    (Some(_), Some(_)) => {
                                        ui.label("OFF");
                                    }
                                    _ => {
                                        ui.weak("—");
                                    }
                                }
                            }
                            ui.end_row();
                        } else if let (false, Some(pull)) = (map == Map::Vanos, &self.pull) {
                            ui.label(if fuel {
                                "AFR (last pull)"
                            } else {
                                "Knock (last pull)"
                            });
                            for &rpm in &RPM_AXIS {
                                match pull.iter().find(|s| s.rpm == rpm) {
                                    Some(s) if fuel => {
                                        let (lo, hi) = WOT_BAND;
                                        let text = format!("{:.1}", s.afr);
                                        if s.afr > LEAN_WARNING {
                                            ui.colored_label(FAIL, text);
                                        } else if s.afr < lo || s.afr > hi {
                                            ui.colored_label(WARN, text);
                                        } else {
                                            ui.colored_label(PASS, text);
                                        }
                                    }
                                    Some(s) if s.knocking() => {
                                        ui.colored_label(FAIL, format!("-{:.1}°", s.knock_retard));
                                    }
                                    Some(_) => {
                                        ui.colored_label(PASS, "0");
                                    }
                                    None => {
                                        ui.weak("—");
                                    }
                                }
                            }
                            ui.end_row();
                        }
                    });
            });
        ui.label(
            egui::RichText::new(
                "Click cells to select them, or click an RPM or load heading to select a whole \
                 column or row. A dyno pull is full throttle, so it reads the 100% row.",
            )
            .small()
            .weak(),
        );

        match click {
            Some(Click::Cell(r, c)) => {
                if !self.selected.remove(&(r, c)) {
                    self.selected.insert((r, c));
                }
            }
            Some(Click::Row(r)) => {
                toggle_group(&mut self.selected, (0..Table::COLS).map(|c| (r, c)))
            }
            Some(Click::Col(c)) => {
                toggle_group(&mut self.selected, (0..Table::ROWS).map(|r| (r, c)))
            }
            None => {}
        }
    }

    fn charts(&self, ui: &mut egui::Ui) {
        let pull = self.pull.as_deref().unwrap_or_default();
        let prev = self.previous.as_deref();
        let series = |data: &[Sample], f: fn(&Sample) -> f64| -> PlotPoints {
            data.iter().map(|s| [s.rpm, f(s)]).collect()
        };

        Plot::new("practice_afr")
            .height(150.0)
            .link_axis("practice", [true, false])
            .legend(Legend::default())
            .include_y(11.0)
            .include_y(15.0)
            .y_axis_label("AFR")
            .y_axis_min_width(48.0)
            .allow_drag(false)
            .allow_scroll(false)
            .show(ui, |plot| {
                let (lo, hi) = WOT_BAND;
                plot.hline(
                    HLine::new("Target band", lo)
                        .color(PASS)
                        .style(LineStyle::dashed_loose()),
                );
                plot.hline(
                    // Unnamed so the legend shows the band once.
                    HLine::new("", hi)
                        .color(PASS)
                        .style(LineStyle::dashed_loose()),
                );
                if let Some(prev) = prev {
                    plot.line(Line::new("Last run", series(prev, |s| s.afr)).color(PREVIOUS));
                }
                plot.line(Line::new("AFR", series(pull, |s| s.afr)).color(WARN));
            });
        Plot::new("practice_power")
            .height(170.0)
            .link_axis("practice", [true, false])
            .legend(Legend::default())
            .include_y(0.0)
            .y_axis_label("bhp / Nm")
            .y_axis_min_width(48.0)
            .allow_drag(false)
            .allow_scroll(false)
            .show(ui, |plot| {
                if let Some(prev) = prev {
                    plot.line(Line::new("Last run", series(prev, |s| s.power_bhp)).color(PREVIOUS));
                }
                plot.line(Line::new("Power (bhp)", series(pull, |s| s.power_bhp)));
                plot.line(Line::new("Torque (Nm)", series(pull, |s| s.torque_nm)));
            });
        Plot::new("practice_knock")
            .height(110.0)
            .link_axis("practice", [true, false])
            .legend(Legend::default())
            .include_y(0.0)
            .include_y(6.0)
            .x_axis_label("rpm")
            .y_axis_label("knock (°)")
            .y_axis_min_width(48.0)
            .allow_drag(false)
            .allow_scroll(false)
            .show(ui, |plot| {
                plot.line(
                    Line::new("Timing pulled by knock", series(pull, |s| s.knock_retard))
                        .color(FAIL)
                        .fill(0.0),
                );
            });
    }
}

/// Torque with VANOS forced on and off, to find where to switch.
fn cam_chart(ui: &mut egui::Ui, on: &[Sample], off: &[Sample]) {
    ui.add_space(6.0);
    ui.strong("Cam position comparison (full throttle)");
    let torque = |p: &[Sample]| -> PlotPoints { p.iter().map(|s| [s.rpm, s.torque_nm]).collect() };
    Plot::new("practice_cam")
        .height(170.0)
        .legend(Legend::default())
        .x_axis_label("rpm")
        .y_axis_label("torque (Nm)")
        .y_axis_min_width(48.0)
        .allow_drag(false)
        .allow_scroll(false)
        .show(ui, |plot| {
            plot.line(Line::new("VANOS ON (cam advanced)", torque(on)).color(PASS));
            plot.line(Line::new("VANOS OFF", torque(off)).color(PREVIOUS));
        });
    ui.label(
        egui::RichText::new(
            "Where the green line is higher, VANOS ON makes more torque. Switch it off where the \
             lines cross, and keep it off at idle.",
        )
        .small()
        .weak(),
    );
}

fn all_cells() -> impl Iterator<Item = (usize, usize)> {
    (0..Table::ROWS).flat_map(|r| (0..Table::COLS).map(move |c| (r, c)))
}

/// Selects the whole group, or deselects it if it was all selected already.
fn toggle_group(
    selected: &mut BTreeSet<(usize, usize)>,
    cells: impl Iterator<Item = (usize, usize)>,
) {
    let cells: Vec<_> = cells.collect();
    if cells.iter().all(|c| selected.contains(c)) {
        for c in &cells {
            selected.remove(c);
        }
    } else {
        selected.extend(cells);
    }
}

fn peak_by(pull: &[Sample], f: fn(&Sample) -> f64) -> Sample {
    *pull
        .iter()
        .max_by(|a, b| f(a).total_cmp(&f(b)))
        .expect("pull has samples")
}

/// Blue (low) through yellow to red (high).
fn heat(v: f64, lo: f64, hi: f64) -> egui::Color32 {
    let t = if hi > lo {
        ((v - lo) / (hi - lo)) as f32
    } else {
        0.5
    };
    let stops = [(110, 160, 235), (240, 210, 90), (235, 110, 80)];
    let (a, b, t) = if t < 0.5 {
        (stops[0], stops[1], t * 2.0)
    } else {
        (stops[1], stops[2], (t - 0.5) * 2.0)
    };
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    egui::Color32::from_rgb(mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_heading_selects_then_deselects_the_row() {
        let mut sel = BTreeSet::new();
        toggle_group(&mut sel, (0..Table::COLS).map(|c| (9, c)));
        assert_eq!(sel.len(), Table::COLS);
        toggle_group(&mut sel, (0..Table::COLS).map(|c| (9, c)));
        assert!(sel.is_empty());
    }

    #[test]
    fn correcting_the_lean_scenario_from_a_pull_completes_it() {
        let mut p = Practice::new();
        p.run_pull();
        assert!(p.checks.as_ref().unwrap().iter().any(|c| !c.ok));
        p.selected = (0..Table::COLS).map(|c| (9, c)).collect();
        let pull = p.pull.clone().unwrap();
        p.edit(|t, sel| {
            for &(r, c) in sel {
                if let Some(s) = pull.iter().find(|s| s.rpm == RPM_AXIS[c]) {
                    t.set(r, c, t.get(r, c) * fuel_correction(s.afr, POWER_AFR));
                }
            }
        });
        p.run_pull();
        assert!(p.checks.as_ref().unwrap().iter().all(|c| c.ok));
        assert!(p.previous.is_some());
    }

    #[test]
    fn undo_restores_the_previous_maps() {
        let mut p = Practice::new();
        let before = p.tune.clone();
        p.selected.insert((0, 0));
        p.edit(|t, _| t.map(|v| v + 1.0));
        assert_ne!(p.tune, before);
        p.tune = p.undo.pop().unwrap();
        assert_eq!(p.tune, before);
    }

    #[test]
    fn heat_runs_blue_to_red() {
        assert_eq!(heat(0.0, 0.0, 1.0), egui::Color32::from_rgb(110, 160, 235));
        assert_eq!(heat(1.0, 0.0, 1.0), egui::Color32::from_rgb(235, 110, 80));
    }
}
