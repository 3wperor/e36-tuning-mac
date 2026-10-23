//! A simulated BMW M52 engine for practising tuning. Nothing here talks to a
//! car; it exists so you can learn what to change, and what happens when you
//! change it, without risking an engine.
//!
//! The model is deliberately simple but behaves the right way round: more
//! fuel means a richer mixture, lean mixtures knock sooner, too much timing
//! knocks and the ECU pulls it back, too little timing loses power, and a
//! vacuum leak shows up as big positive fuel trims at idle that fade with load.
//! The numbers are plausible for an M52B25, not measurements of one.
//!
//! - [`Table`]: a fuel or ignition map (RPM across, load down)
//! - [`simulate`], [`dyno_pull`], [`snapshots`]: run the engine
//! - [`scenarios`], [`assess`]: the practice exercises and their checks

/// Engine speed axis of every map (columns).
pub const RPM_AXIS: [f64; 12] = [
    1000.0, 1500.0, 2000.0, 2500.0, 3000.0, 3500.0, 4000.0, 4500.0, 5000.0, 5500.0, 6000.0, 6500.0,
];
/// Load axis of every map, in percent (rows).
pub const LOAD_AXIS: [f64; 10] = [10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0];

/// Air/fuel ratio of a perfectly balanced (stoichiometric) mixture.
pub const STOICH: f64 = 14.7;
/// Full-throttle air/fuel ratio the scenarios aim for.
pub const POWER_AFR: f64 = 12.8;
/// Acceptable full-throttle air/fuel range.
pub const WOT_BAND: (f64, f64) = (12.5, 13.2);
/// Leaner than this at high load is a warning; much leaner is dangerous.
pub const LEAN_WARNING: f64 = 13.6;

/// Most a closed-loop fuel trim can correct, as a fraction.
const TRIM_LIMIT: f64 = 0.25;
/// Injection time (ms) for a stoichiometric mixture per unit of air.
const FUEL_K: f64 = 13.0;
/// Torque (Nm) per unit of air at best timing and mixture.
const TORQUE_K: f64 = 270.0;
/// How much the ECU pulls beyond the knock threshold when it hears knock.
const KNOCK_PULL: f64 = 3.0;

/// A map: one value per (load row, RPM column).
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    values: Vec<f64>,
}

impl Table {
    pub const ROWS: usize = LOAD_AXIS.len();
    pub const COLS: usize = RPM_AXIS.len();

    pub fn from_fn(f: impl Fn(f64, f64) -> f64) -> Self {
        let mut values = Vec::with_capacity(Self::ROWS * Self::COLS);
        for &load in &LOAD_AXIS {
            for &rpm in &RPM_AXIS {
                values.push(f(rpm, load));
            }
        }
        Self { values }
    }

    pub fn get(&self, row: usize, col: usize) -> f64 {
        self.values[row * Self::COLS + col]
    }

    pub fn set(&mut self, row: usize, col: usize, value: f64) {
        self.values[row * Self::COLS + col] = value;
    }

    pub fn map(&mut self, f: impl Fn(f64) -> f64) {
        for v in &mut self.values {
            *v = f(*v);
        }
    }

    pub fn min_max(&self) -> (f64, f64) {
        self.values
            .iter()
            .fold((f64::MAX, f64::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)))
    }

    /// The value at any RPM and load, blending the four surrounding cells.
    /// Outside the axes it uses the nearest edge.
    pub fn lookup(&self, rpm: f64, load: f64) -> f64 {
        let (c0, c1, tc) = locate(&RPM_AXIS, rpm);
        let (r0, r1, tr) = locate(&LOAD_AXIS, load);
        let top = lerp(self.get(r0, c0), self.get(r0, c1), tc);
        let bottom = lerp(self.get(r1, c0), self.get(r1, c1), tc);
        lerp(top, bottom, tr)
    }
}

fn locate(axis: &[f64], x: f64) -> (usize, usize, f64) {
    let last = axis.len() - 1;
    if x <= axis[0] {
        return (0, 0, 0.0);
    }
    if x >= axis[last] {
        return (last, last, 0.0);
    }
    let i = axis.windows(2).position(|w| x < w[1]).unwrap_or(last - 1);
    (i, i + 1, (x - axis[i]) / (axis[i + 1] - axis[i]))
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// The ECU's maps that practice lets you edit.
#[derive(Debug, Clone, PartialEq)]
pub struct Tune {
    /// Injection time in milliseconds.
    pub fuel: Table,
    /// Ignition advance in degrees before top dead centre.
    pub ignition: Table,
}

/// Things about the engine itself, which no map change can fix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Engine {
    /// Air getting in past the air flow meter (a vacuum leak), in the same
    /// units as the engine's airflow (full throttle at peak torque is ~0.92).
    pub vacuum_leak: f64,
    /// How much fuel the injectors actually flow compared with what the maps
    /// were made for (1.2 = 20% bigger injectors).
    pub injector_flow: f64,
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            vacuum_leak: 0.0,
            injector_flow: 1.0,
        }
    }
}

// The engine's hidden physics. Practice never shows these directly.

/// Volumetric efficiency: how well the engine breathes at each speed.
fn ve(rpm: f64) -> f64 {
    0.72 + 0.20 * (-((rpm - 4100.0) / 2300.0).powi(2)).exp()
        - 0.10 * ((rpm - 5000.0) / 1500.0).max(0.0).powi(2)
}

fn airflow(rpm: f64, load: f64) -> f64 {
    ve(rpm) * load / 100.0
}

/// Ignition advance that gives the most torque.
fn best_timing(rpm: f64, load: f64) -> f64 {
    14.0 + 12.0 * ((rpm - 1000.0) / 3000.0).clamp(0.0, 1.0)
        + 4.0 * ((rpm - 4000.0) / 2500.0).max(0.0)
        + 18.0 * (1.0 - load / 100.0)
}

/// Advance beyond which the engine knocks. Lowest at high load and low-to-mid
/// revs, and lower still when the mixture is lean.
fn knock_threshold(rpm: f64, load: f64, afr: f64) -> f64 {
    let susceptibility = 1.0 - 0.6 * ((rpm - 3500.0) / 3000.0).clamp(0.0, 1.0);
    best_timing(rpm, load) + 4.0
        - 9.0 * (load / 100.0).powi(2) * susceptibility
        - 2.0 * (afr - 13.0).max(0.0)
}

/// Where the ECU uses its O2 sensors to correct the mixture.
pub fn is_closed_loop(rpm: f64, load: f64) -> bool {
    load <= 60.0 && rpm <= 4500.0
}

/// Air/fuel ratio the stock maps were made for.
fn stock_target_afr(rpm: f64, load: f64) -> f64 {
    if is_closed_loop(rpm, load) {
        STOICH
    } else if load >= 80.0 {
        POWER_AFR
    } else {
        13.5
    }
}

fn round_to(x: f64, step: f64) -> f64 {
    (x / step).round() * step
}

/// Maps that suit a healthy, standard engine.
pub fn stock_tune() -> Tune {
    Tune {
        fuel: Table::from_fn(|rpm, load| {
            round_to(
                FUEL_K * airflow(rpm, load) * STOICH / stock_target_afr(rpm, load),
                0.01,
            )
        }),
        ignition: Table::from_fn(|rpm, load| {
            let afr = stock_target_afr(rpm, load);
            round_to(
                best_timing(rpm, load).min(knock_threshold(rpm, load, afr) - 1.5),
                0.5,
            )
        }),
    }
}

/// What the engine does at one RPM and load.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub rpm: f64,
    pub load: f64,
    /// What a wideband would read.
    pub afr: f64,
    pub closed_loop: bool,
    /// Closed-loop fuel trim the ECU applied (+0.10 = adding 10%).
    pub trim: f64,
    /// Advance from the ignition map.
    pub timing_map: f64,
    /// How much the ECU pulled because it heard knock.
    pub knock_retard: f64,
    /// Advance actually used.
    pub timing: f64,
    pub torque_nm: f64,
    pub power_bhp: f64,
}

impl Sample {
    pub fn knocking(&self) -> bool {
        self.knock_retard > 0.0
    }
}

pub fn simulate(engine: &Engine, tune: &Tune, rpm: f64, load: f64) -> Sample {
    let air = airflow(rpm, load) + engine.vacuum_leak;
    let needed = FUEL_K * air;
    let base = tune.fuel.lookup(rpm, load) * engine.injector_flow;
    let closed_loop = is_closed_loop(rpm, load);
    let trim = if closed_loop {
        (needed / base - 1.0).clamp(-TRIM_LIMIT, TRIM_LIMIT)
    } else {
        0.0
    };
    let afr = STOICH * needed / (base * (1.0 + trim));

    let timing_map = tune.ignition.lookup(rpm, load);
    let threshold = knock_threshold(rpm, load, afr);
    let knock_retard = if timing_map > threshold {
        timing_map - threshold + KNOCK_PULL
    } else {
        0.0
    };
    let timing = timing_map - knock_retard;

    let timing_eff = (1.0 - 0.0025 * (timing - best_timing(rpm, load)).powi(2)).max(0.5);
    let mixture_eff = (1.0 - 0.012 * (afr - POWER_AFR).powi(2)).max(0.5);
    let torque_nm = TORQUE_K * air * timing_eff * mixture_eff;
    Sample {
        rpm,
        load,
        afr,
        closed_loop,
        trim,
        timing_map,
        knock_retard,
        timing,
        torque_nm,
        power_bhp: torque_nm * rpm / 7120.9,
    }
}

/// A full-throttle run from 1,500 to 6,500 rpm, like on a dyno.
pub fn dyno_pull(engine: &Engine, tune: &Tune) -> Vec<Sample> {
    (15..=65)
        .map(|r| simulate(engine, tune, r as f64 * 100.0, 100.0))
        .collect()
}

/// Everyday driving conditions, for reading fuel trims.
pub const SNAPSHOT_POINTS: [(&str, f64, f64); 5] = [
    ("Idle", 800.0, 12.0),
    ("Cruise, 50 mph", 2200.0, 25.0),
    ("Cruise, 70 mph", 3000.0, 35.0),
    ("Part throttle", 3500.0, 55.0),
    ("Full throttle", 4000.0, 100.0),
];

pub fn snapshots(engine: &Engine, tune: &Tune) -> Vec<(&'static str, Sample)> {
    SNAPSHOT_POINTS
        .iter()
        .map(|&(name, rpm, load)| (name, simulate(engine, tune, rpm, load)))
        .collect()
}

/// Average power across the useful rev range of a pull.
pub fn average_power(pull: &[Sample]) -> f64 {
    let useful: Vec<f64> = pull
        .iter()
        .filter(|s| s.rpm >= 2000.0)
        .map(|s| s.power_bhp)
        .collect();
    useful.iter().sum::<f64>() / useful.len().max(1) as f64
}

/// The factor to multiply fuel by so a cell that read `measured` reads
/// `target`: lean (higher AFR) means more fuel.
pub fn fuel_correction(measured_afr: f64, target_afr: f64) -> f64 {
    measured_afr / target_afr
}

// Scenarios

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Edit the maps and run dyno pulls until the goals pass.
    Tuning,
    /// Work out what's wrong and pick the right fix.
    Diagnosis,
    /// No goals; just experiment.
    Sandbox,
}

/// Something a scenario checks after each run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Goal {
    /// Full-throttle AFR inside [`WOT_BAND`] from 2,000 rpm up.
    WotAfrInBand,
    /// No knock anywhere in the pull.
    NoKnock,
    /// Fuel trims in everyday driving within ± this fraction.
    TrimsWithin(f64),
    /// Average power at least this fraction of what the engine can safely make.
    KeepPower(f64),
}

pub struct Question {
    pub prompt: &'static str,
    /// Each answer with the explanation shown after choosing it.
    pub answers: &'static [(&'static str, &'static str)],
    pub correct: usize,
}

pub struct Scenario {
    pub title: &'static str,
    /// What the driver notices.
    pub story: &'static str,
    pub task: &'static str,
    pub hints: &'static [&'static str],
    pub kind: Kind,
    pub engine: Engine,
    pub tune: Tune,
    pub goals: &'static [Goal],
    pub question: Option<Question>,
}

pub fn scenarios() -> Vec<Scenario> {
    vec![
        lean_top_end(),
        too_much_timing(),
        vacuum_leak(),
        bigger_injectors(),
        sandbox(),
    ]
}

fn lean_top_end() -> Scenario {
    let mut tune = stock_tune();
    for (col, &rpm) in RPM_AXIS.iter().enumerate() {
        let lean = match rpm as u32 {
            4500 => 0.95,
            5000 => 0.92,
            5500 => 0.89,
            6000 => 0.87,
            6500 => 0.86,
            _ => continue,
        };
        for row in 7..Table::ROWS {
            tune.fuel
                .set(row, col, round_to(tune.fuel.get(row, col) * lean, 0.01));
        }
    }
    Scenario {
        title: "1. Lean at the top end",
        story: "After a new intake and exhaust, the car feels flat above 4,500 rpm. The \
                wideband shows the mixture going lean at full throttle.",
        task: "Get full-throttle AFR between 12.5 and 13.2 from 2,000 to 6,500 rpm, with no knock.",
        hints: &[
            "Run a dyno pull first, then look at the 'AFR (last pull)' row under the fuel map. \
             Red cells are outside the target.",
            "A dyno pull is full throttle, so it only uses the 100% load row of the map.",
            "Higher AFR means lean, which means more fuel. To go from 14.0 to 12.8 you need \
             14.0 / 12.8 = 1.094, so about 9% more fuel in that cell.",
            "Select the red cells in the 100% row and press 'Correct from last pull': it does \
             that sum for every selected cell. Then run another pull.",
            "Notice the knock disappears once the mixture is right: lean mixtures knock sooner.",
        ],
        kind: Kind::Tuning,
        engine: Engine::default(),
        tune,
        goals: &[Goal::WotAfrInBand, Goal::NoKnock],
        question: None,
    }
}

fn too_much_timing() -> Scenario {
    let mut tune = stock_tune();
    for (col, &rpm) in RPM_AXIS.iter().enumerate() {
        if (2500.0..=4500.0).contains(&rpm) {
            for row in 6..Table::ROWS {
                tune.ignition
                    .set(row, col, tune.ignition.get(row, col) + 5.0);
            }
        }
    }
    Scenario {
        title: "2. Too much timing",
        story: "Someone added ignition timing to 'free up power'. Now the engine pinks under \
                load and actually feels slower.",
        task: "Remove all knock while keeping at least 97% of the power the engine can safely make.",
        hints: &[
            "Switch to the ignition map and run a pull. The 'Knock retard (last pull)' row shows \
             where the ECU is pulling timing.",
            "When the ECU hears knock it pulls more timing than the excess, so too much advance \
             ends up with less timing and less power than the right amount.",
            "Take timing out of the knocking cells in small steps (0.5 to 1 degree) and pull again.",
            "Don't just remove lots of timing everywhere: too little timing loses power, and the \
             power check will fail.",
        ],
        kind: Kind::Tuning,
        engine: Engine::default(),
        tune,
        goals: &[Goal::NoKnock, Goal::KeepPower(0.97)],
        question: None,
    }
}

fn vacuum_leak() -> Scenario {
    Scenario {
        title: "3. Vacuum leak",
        story: "Both O2 sensors have logged faults, the idle is lumpy, and the ECU's fuel trims \
                look high. The maps haven't been touched.",
        task: "Read the fuel trims and choose the right fix.",
        hints: &[
            "Fuel trims are the ECU correcting the mixture with its O2 sensors. +20% means it's \
             adding 20% more fuel than the map says, because the engine is running lean.",
            "Look at how the trims change with load: huge at idle, smaller when cruising.",
            "A leak lets in a fixed amount of air the air flow meter never measured. At idle the \
             engine takes in little air, so that extra is a big share; at full throttle it's tiny.",
        ],
        kind: Kind::Diagnosis,
        engine: Engine {
            vacuum_leak: 0.025,
            ..Engine::default()
        },
        tune: stock_tune(),
        goals: &[],
        question: Some(Question {
            prompt: "What should you do?",
            answers: &[
                (
                    "Add 25% fuel to the idle and cruise cells",
                    "This hides the symptom instead of fixing the cause. The leak's effect \
                     changes with load, temperature and engine speed, so a map tuned around it \
                     will be wrong everywhere else, and wrong again the day the leak is fixed.",
                ),
                (
                    "Find and fix the vacuum leak, then check the trims again",
                    "Right. Trims that are big at idle and shrink as load rises point to unmetered \
                     air. Fix the leak (smoke test, then check the intake boot, crankcase vent \
                     hoses, idle valve hoses and brake booster hose), and the trims return to \
                     near zero. Only tune once the engine is mechanically healthy.",
                ),
                (
                    "Replace both O2 sensors",
                    "The sensors are doing their job: they're reporting a lean engine. Both banks \
                     reading lean at the same time, mostly at idle, points to air getting in, not \
                     two sensors failing together.",
                ),
                (
                    "Add ignition timing at idle to smooth it out",
                    "Timing doesn't fix a lean mixture. The idle is lumpy because it's lean and \
                     the trims have run out of correction.",
                ),
            ],
            correct: 1,
        }),
    }
}

fn bigger_injectors() -> Scenario {
    Scenario {
        title: "4. Bigger injectors",
        story: "Injectors that flow 20% more have been fitted, and the maps weren't changed. It \
                smells rich and the trims are pulling fuel everywhere.",
        task: "Get full-throttle AFR between 12.5 and 13.2, and everyday fuel trims within ±5%.",
        hints: &[
            "Look at the fuel trims below: they're all negative by about the same amount. When \
             everything is off by the same percentage, the fix is the whole table, not single cells.",
            "If the injectors flow 20% more, every cell needs 1 / 1.20 = 0.833 times the fuel, \
             which is about -16.7%.",
            "Use 'Scale whole table' with -16.7, then run a pull and check the trims.",
        ],
        kind: Kind::Tuning,
        engine: Engine {
            injector_flow: 1.2,
            ..Engine::default()
        },
        tune: stock_tune(),
        goals: &[Goal::WotAfrInBand, Goal::TrimsWithin(0.05)],
        question: None,
    }
}

fn sandbox() -> Scenario {
    Scenario {
        title: "5. Free practice",
        story: "A healthy standard engine with standard maps.",
        task: "Change anything and see what happens. Try adding timing until it knocks, or \
               leaning out the top end and watching the knock appear.",
        hints: &[],
        kind: Kind::Sandbox,
        engine: Engine::default(),
        tune: stock_tune(),
        goals: &[],
        question: None,
    }
}

pub struct Check {
    pub ok: bool,
    pub text: String,
}

/// Checks a scenario's goals against the engine with the given maps.
pub fn assess(scenario: &Scenario, engine: &Engine, tune: &Tune) -> Vec<Check> {
    let pull = dyno_pull(engine, tune);
    scenario
        .goals
        .iter()
        .map(|goal| match *goal {
            Goal::WotAfrInBand => {
                let (lo, hi) = WOT_BAND;
                let worst = pull
                    .iter()
                    .filter(|s| s.rpm >= 2000.0)
                    .max_by(|a, b| band_error(a.afr).total_cmp(&band_error(b.afr)));
                match worst {
                    Some(s) if band_error(s.afr) > 0.0 => Check {
                        ok: false,
                        text: format!(
                            "Full-throttle AFR: {:.1} at {:.0} rpm is outside {lo}–{hi} ({}).",
                            s.afr,
                            s.rpm,
                            if s.afr > hi { "lean" } else { "rich" }
                        ),
                    },
                    _ => Check {
                        ok: true,
                        text: format!("Full-throttle AFR stays within {lo}–{hi}."),
                    },
                }
            }
            Goal::NoKnock => {
                let knocking: Vec<&Sample> = pull.iter().filter(|s| s.knocking()).collect();
                match (knocking.first(), knocking.last()) {
                    (Some(first), Some(last)) => Check {
                        ok: false,
                        text: format!(
                            "Knock from {:.0} to {:.0} rpm (up to {:.1}° pulled).",
                            first.rpm,
                            last.rpm,
                            knocking.iter().map(|s| s.knock_retard).fold(0.0, f64::max)
                        ),
                    },
                    _ => Check {
                        ok: true,
                        text: "No knock anywhere in the pull.".into(),
                    },
                }
            }
            Goal::TrimsWithin(limit) => {
                let snaps = snapshots(engine, tune);
                let worst = snaps
                    .iter()
                    .filter(|(_, s)| s.closed_loop)
                    .max_by(|a, b| a.1.trim.abs().total_cmp(&b.1.trim.abs()));
                match worst {
                    Some((name, s)) if s.trim.abs() > limit => Check {
                        ok: false,
                        text: format!(
                            "Fuel trim {:+.0}% at '{name}' (needs to be within ±{:.0}%).",
                            s.trim * 100.0,
                            limit * 100.0
                        ),
                    },
                    _ => Check {
                        ok: true,
                        text: format!("Everyday fuel trims within ±{:.0}%.", limit * 100.0),
                    },
                }
            }
            Goal::KeepPower(share) => {
                let reference = average_power(&dyno_pull(engine, &stock_tune()));
                let now = average_power(&pull);
                Check {
                    ok: now >= share * reference,
                    text: format!(
                        "Average power {:.0} bhp: {:.0}% of the {:.0} bhp the engine can safely \
                         make (needs {:.0}%).",
                        now,
                        100.0 * now / reference,
                        reference,
                        share * 100.0
                    ),
                }
            }
        })
        .collect()
}

fn band_error(afr: f64) -> f64 {
    let (lo, hi) = WOT_BAND;
    (lo - afr).max(afr - hi).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_ok(checks: &[Check]) -> bool {
        checks.iter().all(|c| c.ok)
    }

    fn scenario(i: usize) -> Scenario {
        scenarios().swap_remove(i)
    }

    #[test]
    fn lookup_blends_cells_and_clamps_at_edges() {
        let t = Table::from_fn(|rpm, load| rpm / 1000.0 + load);
        assert!((t.lookup(1250.0, 10.0) - 11.25).abs() < 1e-9);
        assert!((t.lookup(1000.0, 15.0) - 16.0).abs() < 1e-9);
        assert_eq!(t.lookup(500.0, 0.0), t.get(0, 0));
        assert_eq!(t.lookup(9000.0, 150.0), t.get(9, 11));
    }

    #[test]
    fn stock_engine_is_healthy() {
        let (engine, tune) = (Engine::default(), stock_tune());
        let pull = dyno_pull(&engine, &tune);
        assert!(pull.iter().all(|s| !s.knocking()));
        assert!(pull.iter().all(|s| (s.afr - POWER_AFR).abs() < 0.1));
        for (_, s) in snapshots(&engine, &tune) {
            assert!(s.trim.abs() < 0.02, "trim {}", s.trim);
        }
        let peak = pull
            .iter()
            .max_by(|a, b| a.power_bhp.total_cmp(&b.power_bhp))
            .unwrap();
        assert!(
            (170.0..=210.0).contains(&peak.power_bhp),
            "peak {} bhp",
            peak.power_bhp
        );
        assert!(
            (5000.0..=6200.0).contains(&peak.rpm),
            "power should peak before the redline, not at {} rpm",
            peak.rpm
        );
    }

    #[test]
    fn lean_scenario_fails_then_passes_after_correcting_fuel() {
        let s = scenario(0);
        let checks = assess(&s, &s.engine, &s.tune);
        assert!(!checks[0].ok, "should start lean");
        assert!(!checks[1].ok, "lean should cause knock");

        let pull = dyno_pull(&s.engine, &s.tune);
        let mut tune = s.tune.clone();
        for (col, &rpm) in RPM_AXIS.iter().enumerate() {
            if let Some(p) = pull.iter().find(|p| p.rpm == rpm) {
                let v = tune.fuel.get(9, col) * fuel_correction(p.afr, POWER_AFR);
                tune.fuel.set(9, col, v);
            }
        }
        assert!(all_ok(&assess(&s, &s.engine, &tune)));
    }

    #[test]
    fn timing_scenario_needs_the_right_amount_of_timing() {
        let s = scenario(1);
        assert!(
            !assess(&s, &s.engine, &s.tune)[0].ok,
            "should start knocking"
        );

        let fixed = Tune {
            ignition: stock_tune().ignition,
            ..s.tune.clone()
        };
        assert!(all_ok(&assess(&s, &s.engine, &fixed)));

        let mut too_safe = fixed.clone();
        too_safe.ignition.map(|v| v - 8.0);
        let checks = assess(&s, &s.engine, &too_safe);
        assert!(
            checks[0].ok && !checks[1].ok,
            "over-retarding should lose power"
        );
    }

    #[test]
    fn vacuum_leak_shows_as_trims_that_fade_with_load() {
        let s = scenario(2);
        let snaps = snapshots(&s.engine, &s.tune);
        let idle = snaps[0].1;
        let cruise = snaps[2].1;
        assert!(idle.trim >= 0.24, "idle trim {}", idle.trim);
        assert!(cruise.trim > 0.05 && cruise.trim < idle.trim);

        let fixed = Engine::default();
        for (_, s) in snapshots(&fixed, &s.tune) {
            assert!(s.trim.abs() < 0.02);
        }
        assert_eq!(s.question.as_ref().unwrap().correct, 1);
    }

    #[test]
    fn injector_scenario_is_fixed_by_scaling_the_table() {
        let s = scenario(3);
        assert!(!all_ok(&assess(&s, &s.engine, &s.tune)));
        let mut tune = s.tune.clone();
        tune.fuel.map(|v| v / 1.2);
        assert!(all_ok(&assess(&s, &s.engine, &tune)));
    }

    #[test]
    fn more_timing_than_the_threshold_ends_with_less_power() {
        let (engine, tune) = (Engine::default(), stock_tune());
        let mut advanced = tune.clone();
        advanced.ignition.map(|v| v + 6.0);
        let normal = simulate(&engine, &tune, 3000.0, 100.0);
        let pushed = simulate(&engine, &advanced, 3000.0, 100.0);
        assert!(pushed.knocking());
        assert!(pushed.timing < normal.timing);
        assert!(pushed.torque_nm < normal.torque_nm);
    }

    #[test]
    fn sandbox_has_no_goals() {
        let s = scenario(4);
        assert!(assess(&s, &s.engine, &s.tune).is_empty());
    }
}
