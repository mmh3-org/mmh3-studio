//! Whether macOS lets the GPU run at full speed. A Mac outside High Power Mode, on battery or
//! held back for heat keeps the GPU at a low clock, and a generation takes two or three times
//! as long. A small adapter or another process keeping the CPU busy takes a share of the power
//! the GPU runs on.
//!
//! Everything is read from `pmset`, `sysctl`, `ps` and `NSProcessInfo`, none of which needs
//! root.

use std::time::{Duration, Instant};

/// The Energy Mode macOS runs in: `pmset`'s `powermode` 0, 1 and 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Automatic,
    Low,
    High,
}

/// `NSProcessInfoThermalState`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Thermal {
    #[default]
    Nominal,
    Fair,
    Serious,
    Critical,
}

/// How much power the chip wants.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Chip {
    #[default]
    Other,
    /// A Max or Ultra chip, which wants at least a 96 W adapter. `capped_case` is a 14-inch
    /// laptop whose case caps its power and cooling below what the chip can draw, whatever the
    /// adapter.
    Large { capped_case: bool },
}

/// Where the Mac draws its power from, from `pmset -g ps`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Battery,
    /// `watts` is the adapter's rating when `pmset -g ac` gives one; `draining` is a battery
    /// running down although the adapter is plugged in, so the adapter can't keep up.
    Adapter {
        watts: Option<u32>,
        draining: bool,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Power {
    /// `None` on a Mac without Energy Modes.
    pub mode: Option<Mode>,
    /// The Mac offers High Power Mode, so Automatic leaves speed behind.
    pub high_available: bool,
    pub chip: Chip,
    pub low_power: bool,
    pub thermal: Thermal,
    pub source: Option<Source>,
    /// The name of another process that has kept the CPU busy since the last poll.
    pub busy: Option<String>,
}

/// The smallest adapter recommended for a laptop with a Max or Ultra chip.
pub const RECOMMENDED_WATTS: u32 = 96;

/// What holds the GPU back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Issue {
    LowPower,
    NotHighPower,
    Battery,
    Draining,
    SmallAdapter { watts: u32, capped_case: bool },
    Hot(Thermal),
    Busy(String),
}

impl Issue {
    pub fn describe(&self) -> String {
        match self {
            Issue::LowPower => "Low Power Mode is on.".into(),
            Issue::NotHighPower => "Energy Mode is Automatic, not High Power.".into(),
            Issue::Battery => "The Mac is running on battery.".into(),
            Issue::Draining => "The battery is draining although the adapter is plugged in.".into(),
            Issue::SmallAdapter { watts, capped_case: false } => {
                format!("The {watts} W adapter is under the {RECOMMENDED_WATTS} W recommended for a Max or Ultra chip.")
            }
            Issue::SmallAdapter { watts, capped_case: true } => format!(
                "The {watts} W adapter is under the {RECOMMENDED_WATTS} W recommended for this Mac, though the power and cooling of a 14-inch MacBook Pro limit its GPU whatever the adapter."
            ),
            Issue::Hot(Thermal::Critical) => "macOS reports a critical thermal state.".into(),
            Issue::Hot(_) => "macOS reports a serious thermal state.".into(),
            Issue::Busy(name) => format!("{name} is using a lot of CPU, which takes power from the GPU; closing it can speed up generation."),
        }
    }

    /// Fixed by switching Energy Mode, which the copied command does.
    pub fn is_mode(&self) -> bool {
        matches!(self, Issue::LowPower | Issue::NotHighPower)
    }

    /// Makes macOS hold the GPU at a low clock. A small adapter or a busy process costs a share
    /// of the power at most, not the two-to-three-fold slowdown of a low clock.
    pub fn is_clock(&self) -> bool {
        matches!(self, Issue::LowPower | Issue::NotHighPower | Issue::Battery | Issue::Draining | Issue::Hot(_))
    }
}

/// Runs the GPU at full clock in every power state.
pub const HIGH_POWER_COMMAND: &str = "sudo pmset -a powermode 2";

/// The Battery pane of System Settings, which holds Energy Mode.
pub const ENERGY_SETTINGS_URL: &str = "x-apple.systempreferences:com.apple.Battery-Settings.extension";

impl Power {
    pub fn issues(&self) -> Vec<Issue> {
        let mut issues = Vec::new();
        if self.low_power || self.mode == Some(Mode::Low) {
            issues.push(Issue::LowPower);
        } else if self.mode == Some(Mode::Automatic) && self.high_available {
            issues.push(Issue::NotHighPower);
        }
        match self.source {
            Some(Source::Battery) => issues.push(Issue::Battery),
            Some(Source::Adapter { watts, draining }) => {
                if draining {
                    issues.push(Issue::Draining);
                }
                if let (Chip::Large { capped_case }, Some(watts)) = (self.chip, watts.filter(|w| *w < RECOMMENDED_WATTS)) {
                    issues.push(Issue::SmallAdapter { watts, capped_case });
                }
            }
            None => {}
        }
        if matches!(self.thermal, Thermal::Serious | Thermal::Critical) {
            issues.push(Issue::Hot(self.thermal));
        }
        if let Some(name) = &self.busy {
            issues.push(Issue::Busy(name.clone()));
        }
        issues
    }

    /// The status bar's words: the Energy Mode, then the power source.
    pub fn summary(&self) -> String {
        let mode = match (self.low_power, self.mode) {
            (true, _) | (_, Some(Mode::Low)) => Some("Low Power Mode"),
            (_, Some(Mode::High)) => Some("High Power Mode"),
            (_, Some(Mode::Automatic)) => Some("Automatic power"),
            (_, None) => None,
        };
        let source = match self.source {
            Some(Source::Battery) => Some("on battery".to_owned()),
            Some(Source::Adapter { watts: Some(w), .. }) => Some(format!("{w} W adapter")),
            Some(Source::Adapter { watts: None, .. }) => Some("on power".to_owned()),
            None => None,
        };
        let thermal = match self.thermal {
            Thermal::Nominal => None,
            Thermal::Fair => Some("warm"),
            Thermal::Serious => Some("hot"),
            Thermal::Critical => Some("very hot"),
        };
        let parts: Vec<String> = [mode.map(str::to_owned), source, thermal.map(str::to_owned)].into_iter().flatten().collect();
        parts.join(", ")
    }
}

/// The Energy Mode in use, from `pmset -g`. macOS before Sonoma names it with the
/// `lowpowermode` and `highpowermode` switches instead of `powermode`.
pub fn parse_mode(pmset_g: &str) -> Option<Mode> {
    let (mut low, mut high) = (None, None);
    for line in pmset_g.lines() {
        let mut words = line.split_whitespace();
        let (Some(key), Some(value)) = (words.next(), words.next()) else { continue };
        match key {
            "powermode" => {
                return match value {
                    "0" => Some(Mode::Automatic),
                    "1" => Some(Mode::Low),
                    "2" => Some(Mode::High),
                    _ => None,
                };
            }
            "lowpowermode" => low = Some(value == "1"),
            "highpowermode" => high = Some(value == "1"),
            _ => {}
        }
    }
    match (low, high) {
        (None, None) => None,
        (Some(true), _) => Some(Mode::Low),
        (_, Some(true)) => Some(Mode::High),
        _ => Some(Mode::Automatic),
    }
}

/// Whether `pmset -g cap` offers High Power Mode.
pub fn parse_high_available(pmset_cap: &str) -> bool {
    pmset_cap.lines().any(|l| l.trim() == "highpowermode")
}

/// The power source from `pmset -g ps`, with the adapter's watts from `pmset -g ac`.
pub fn parse_source(pmset_ps: &str, pmset_ac: &str) -> Option<Source> {
    let mut lines = pmset_ps.lines();
    let from = lines.next()?.strip_prefix("Now drawing from '")?.strip_suffix('\'')?;
    if from == "Battery Power" {
        return Some(Source::Battery);
    }
    let draining = lines.any(|l| l.split(';').nth(1).is_some_and(|s| s.trim() == "discharging"));
    Some(Source::Adapter { watts: parse_watts(pmset_ac), draining })
}

fn parse_watts(pmset_ac: &str) -> Option<u32> {
    pmset_ac.lines().find_map(|l| l.trim().strip_prefix("Wattage = ")?.strip_suffix('W')?.parse().ok())
}

/// Whether `machdep.cpu.brand_string` names a chip that wants the large adapter.
pub fn parse_large_chip(brand: &str) -> bool {
    let brand = brand.trim();
    brand.ends_with(" Max") || brand.ends_with(" Ultra")
}

/// The 14-inch model with an M5 Pro or M5 Max chip, as `hw.model` names it on Apple's support
/// page that identifies each model. Other models are left out until their limit is known.
const CAPPED_CASES: &[&str] = &["Mac17,7", "Mac17,9"];

/// Whether `hw.model` names a laptop whose case caps a Max chip's power whatever the adapter.
pub fn parse_capped_case(hw_model: &str) -> bool {
    CAPPED_CASES.contains(&hw_model.trim())
}

/// A process seen in `ps -Ao pid,pcpu,comm`.
#[derive(Clone, Debug, PartialEq)]
pub struct Process {
    pub pid: u32,
    pub cpu: f32,
    /// The app's name for a process inside an app bundle, else the executable's file name.
    pub name: String,
}

/// The CPU share, in percent of one core, above which a process counts as heavy.
pub const BUSY_CPU: f32 = 50.0;

/// The processes above [`BUSY_CPU`] that the user could close: not `skip_pid` (the studio), not
/// mmh3, and not part of macOS.
pub fn parse_busy(ps: &str, skip_pid: u32) -> Vec<Process> {
    ps.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, rest) = line.split_once(char::is_whitespace)?;
            let rest = rest.trim_start();
            let (cpu, comm) = rest.split_once(char::is_whitespace)?;
            let (pid, cpu) = (pid.parse().ok()?, cpu.parse().ok()?);
            let comm = comm.trim();
            if cpu <= BUSY_CPU || pid == skip_pid {
                return None;
            }
            Some(Process { pid, cpu, name: closable_name(comm)? })
        })
        .filter(|p| p.name != "mmh3")
        .collect()
}

/// What to call a process the user could close, or `None` for part of macOS. `WebKit` runs every
/// web page, Safari's included, from /System/Library, and Safari and the other bundled apps
/// live under a /System/Applications folder, so those count as the user's.
fn closable_name(comm: &str) -> Option<String> {
    let file = comm.rsplit('/').next().unwrap_or(comm);
    if ["com.apple.WebKit.WebContent", "com.apple.WebKit.GPU"].iter().any(|web| file.starts_with(web)) {
        return Some("A web page".into());
    }
    let system = comm == "kernel_task"
        || (comm.starts_with("/System/") && !comm.contains("/System/Applications/"))
        || ["/sbin/", "/usr/sbin/", "/usr/libexec/"].iter().any(|dir| comm.starts_with(dir));
    if system {
        return None;
    }
    let app = comm.split('/').find_map(|part| part.strip_suffix(".app")).filter(|name| !name.is_empty());
    Some(app.unwrap_or(file).to_owned())
}

/// The busiest process heavy in both samples.
fn sustained(before: &[Process], now: &[Process]) -> Option<Process> {
    now.iter().filter(|p| before.iter().any(|b| b.pid == p.pid)).max_by(|a, b| a.cpu.total_cmp(&b.cpu)).cloned()
}

/// How far apart two samples must be for a process heavy in both to count as staying busy.
/// A little under the 30-second poll, as a poll can fire a moment early.
pub const BUSY_SPAN: Duration = Duration::from_secs(25);

/// Names a process that stays busy across two polls, not a moment's spike or a compile that
/// a focus check catches twice within seconds.
#[derive(Default)]
pub struct BusyWatch {
    baseline: Option<(Instant, Vec<Process>)>,
    named: Option<Process>,
}

impl BusyWatch {
    pub fn observe(&mut self, at: Instant, sample: Vec<Process>) -> Option<String> {
        match &self.baseline {
            Some((then, before)) if at.duration_since(*then) >= BUSY_SPAN => {
                self.named = sustained(before, &sample);
                self.baseline = Some((at, sample));
            }
            Some(_) => self.named = self.named.take().filter(|p| sample.iter().any(|s| s.pid == p.pid)),
            None => self.baseline = Some((at, sample)),
        }
        self.named.as_ref().map(|p| p.name.clone())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::{Watch, open_energy_settings, watch};

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::process::Command;
    use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
    use std::time::{Duration, Instant};

    use rune::AppHandle;

    use super::{
        BusyWatch, Chip, ENERGY_SETTINGS_URL, Power, Process, Thermal, parse_busy, parse_capped_case, parse_high_available, parse_large_chip, parse_mode,
        parse_source,
    };
    use crate::state::State;

    const EVERY: Duration = Duration::from_secs(30);

    fn run(program: &str, args: &[&str]) -> String {
        Command::new(program)
            .args(args)
            .env("LC_ALL", "C")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }

    /// What a Mac can't change while the studio runs.
    #[derive(Clone, Copy)]
    struct Machine {
        high_available: bool,
        chip: Chip,
    }

    fn read_machine() -> Machine {
        Machine {
            high_available: parse_high_available(&run("/usr/bin/pmset", &["-g", "cap"])),
            chip: if parse_large_chip(&run("/usr/sbin/sysctl", &["-n", "machdep.cpu.brand_string"])) {
                Chip::Large { capped_case: parse_capped_case(&run("/usr/sbin/sysctl", &["-n", "hw.model"])) }
            } else {
                Chip::Other
            },
        }
    }

    fn read_busy() -> Vec<Process> {
        parse_busy(&run("/bin/ps", &["-Ao", "pid,pcpu,comm"]), std::process::id())
    }

    fn read(machine: Machine, busy: Option<String>) -> Power {
        let (low_power, thermal) = process_info();
        Power {
            mode: parse_mode(&run("/usr/bin/pmset", &["-g"])),
            high_available: machine.high_available,
            chip: machine.chip,
            low_power,
            thermal,
            source: parse_source(&run("/usr/bin/pmset", &["-g", "ps"]), &run("/usr/bin/pmset", &["-g", "ac"])),
            busy,
        }
    }

    #[cfg(target_os = "macos")]
    fn process_info() -> (bool, Thermal) {
        use objc2_foundation::{NSProcessInfo, NSProcessInfoThermalState};
        let info = NSProcessInfo::processInfo();
        let thermal = match info.thermalState() {
            NSProcessInfoThermalState::Fair => Thermal::Fair,
            NSProcessInfoThermalState::Serious => Thermal::Serious,
            NSProcessInfoThermalState::Critical => Thermal::Critical,
            _ => Thermal::Nominal,
        };
        (info.isLowPowerModeEnabled(), thermal)
    }

    #[cfg(not(target_os = "macos"))]
    fn process_info() -> (bool, Thermal) {
        (false, Thermal::Nominal)
    }

    /// Asks for a fresh reading ahead of the next poll.
    pub struct Watch(Sender<()>);

    impl Watch {
        pub fn check(&self) {
            let _ = self.0.send(());
        }
    }

    /// Reads the power state every 30 seconds and whenever asked, landing it when it changes.
    pub fn watch(handle: AppHandle<State>) -> Watch {
        let (tx, rx) = channel();
        let _ = rune::task::spawn("mmh3-power", move || {
            let machine = read_machine();
            let mut last = None;
            let mut busy = BusyWatch::default();
            loop {
                let now = read(machine, busy.observe(Instant::now(), read_busy()));
                if last.as_ref() != Some(&now) {
                    let power = now.clone();
                    handle.update(move |s, _| s.power = Some(power));
                    last = Some(now);
                }
                if !wait(&rx) {
                    break;
                }
            }
        });
        Watch(tx)
    }

    /// `Tx::open_url` opens only web and mail links, so System Settings' own scheme goes
    /// through `open`.
    pub fn open_energy_settings() {
        let _ = rune::task::spawn("mmh3-energy-settings", || {
            let _ = Command::new("/usr/bin/open").arg(ENERGY_SETTINGS_URL).status();
        });
    }

    /// Waits out the poll or a request, taking every request queued meanwhile; false once the
    /// studio has gone.
    fn wait(rx: &Receiver<()>) -> bool {
        match rx.recv_timeout(EVERY) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {
                rx.try_iter().for_each(drop);
                true
            }
            Err(RecvTimeoutError::Disconnected) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G_HIGH: &str = include_str!("../fixtures/pmset/g-high.txt");
    const G_AUTOMATIC: &str = include_str!("../fixtures/pmset/g-automatic.txt");
    const G_LOW: &str = include_str!("../fixtures/pmset/g-low.txt");
    const G_LEGACY_LOW: &str = include_str!("../fixtures/pmset/g-legacy-low.txt");
    const CAP_HIGH: &str = include_str!("../fixtures/pmset/cap-high.txt");
    const CAP_NO_HIGH: &str = include_str!("../fixtures/pmset/cap-no-high.txt");
    const AC_100W: &str = include_str!("../fixtures/pmset/ac-100w.txt");
    const AC_90W: &str = include_str!("../fixtures/pmset/ac-90w.txt");
    const AC_NONE: &str = include_str!("../fixtures/pmset/ac-none.txt");
    const PS_AC: &str = include_str!("../fixtures/pmset/ps-ac.txt");
    const PS_AC_DRAINING: &str = include_str!("../fixtures/pmset/ps-ac-draining.txt");
    const PS_BATTERY: &str = include_str!("../fixtures/pmset/ps-battery.txt");
    const PROCESSES_BUSY: &str = include_str!("../fixtures/ps/busy.txt");
    const PROCESSES_QUIET: &str = include_str!("../fixtures/ps/quiet.txt");
    const STUDIO_PID: u32 = 4411;

    #[test]
    fn the_energy_mode_reads_from_pmset() {
        assert_eq!(parse_mode(G_HIGH), Some(Mode::High));
        assert_eq!(parse_mode(G_AUTOMATIC), Some(Mode::Automatic));
        assert_eq!(parse_mode(G_LOW), Some(Mode::Low));
        assert_eq!(parse_mode(G_LEGACY_LOW), Some(Mode::Low));
        assert_eq!(parse_mode(" lowpowermode 0\n highpowermode 1\n"), Some(Mode::High));
        assert_eq!(parse_mode(" lowpowermode 0\n"), Some(Mode::Automatic));
        assert_eq!(parse_mode(" sleep 1\n displaysleep 10\n"), None);
        assert_eq!(parse_mode(""), None);
    }

    #[test]
    fn high_power_mode_is_offered_only_where_pmset_lists_it() {
        assert!(parse_high_available(CAP_HIGH));
        assert!(!parse_high_available(CAP_NO_HIGH));
    }

    #[test]
    fn the_source_and_the_adapter_read_from_pmset() {
        assert_eq!(parse_source(PS_AC, AC_100W), Some(Source::Adapter { watts: Some(100), draining: false }));
        assert_eq!(parse_source(PS_AC_DRAINING, AC_90W), Some(Source::Adapter { watts: Some(90), draining: true }));
        assert_eq!(parse_source(PS_BATTERY, AC_NONE), Some(Source::Battery));
        assert_eq!(parse_source("Now drawing from 'AC Power'\n", ""), Some(Source::Adapter { watts: None, draining: false }));
        assert_eq!(parse_source("", ""), None);
    }

    #[test]
    fn only_max_and_ultra_chips_want_the_recommended_adapter() {
        assert!(parse_large_chip("Apple M5 Max\n"));
        assert!(parse_large_chip("Apple M2 Ultra"));
        assert!(!parse_large_chip("Apple M5 Pro"));
        assert!(!parse_large_chip(""));
    }

    #[test]
    fn only_the_14_inch_m5_macbook_pro_has_a_capped_case() {
        assert!(parse_capped_case("Mac17,7\n"));
        assert!(parse_capped_case("Mac17,9"));
        assert!(!parse_capped_case("Mac17,6"), "the 16-inch model");
        assert!(!parse_capped_case("Mac17,8"), "the 16-inch model");
        assert!(!parse_capped_case("Mac16,8"), "unverified, so neutral");
        assert!(!parse_capped_case("Mac17,77"));
        assert!(!parse_capped_case(""));
    }

    #[test]
    fn heavy_processes_leave_out_the_studio_mmh3_and_macos() {
        let busy = parse_busy(PROCESSES_BUSY, STUDIO_PID);
        let names: Vec<&str> = busy.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["A web page", "Safari", "Google Chrome", "bun"]);
        assert_eq!(busy[3], Process { pid: 61207, cpu: 101.3, name: "bun".into() });
        assert_eq!(parse_busy(PROCESSES_QUIET, STUDIO_PID), []);
        assert_eq!(parse_busy("", STUDIO_PID), []);
    }

    #[test]
    fn a_process_is_named_once_it_stays_busy_across_two_polls() {
        let busy = parse_busy(PROCESSES_BUSY, STUDIO_PID);
        let quiet = parse_busy(PROCESSES_QUIET, STUDIO_PID);
        let start = Instant::now();
        let poll = |n: u32| start + Duration::from_secs(30) * n;
        let mut watch = BusyWatch::default();
        assert_eq!(watch.observe(poll(0), quiet.clone()), None);
        assert_eq!(watch.observe(poll(1), busy.clone()), None, "a single poll may be a spike");
        assert_eq!(watch.observe(poll(2), busy.clone()).as_deref(), Some("bun"), "the busiest of them");
        assert_eq!(watch.observe(poll(3), busy[2..3].to_vec()).as_deref(), Some("Google Chrome"));
        assert_eq!(watch.observe(poll(4), quiet.clone()), None, "it calmed down");
        let restarted = vec![Process { pid: 70001, ..busy[3].clone() }];
        assert_eq!(watch.observe(poll(5), restarted), None, "a new process with the same name starts over");
    }

    #[test]
    fn a_focus_check_soon_after_a_poll_does_not_count_as_the_second_poll() {
        let busy = parse_busy(PROCESSES_BUSY, STUDIO_PID);
        let quiet = parse_busy(PROCESSES_QUIET, STUDIO_PID);
        let start = Instant::now();
        let at = |secs: u64| start + Duration::from_secs(secs);
        let mut watch = BusyWatch::default();
        assert_eq!(watch.observe(at(0), busy.clone()), None);
        assert_eq!(watch.observe(at(2), busy.clone()), None, "two seconds is no sustained load");
        assert_eq!(watch.observe(at(30), busy.clone()).as_deref(), Some("bun"), "the poll compares with the reading 30 s back");
        assert_eq!(watch.observe(at(33), busy[2..3].to_vec()), None, "a focus check drops a process that has calmed down");
        assert_eq!(watch.observe(at(40), busy.clone()), None, "and never names one on a short span");
        assert_eq!(watch.observe(at(60), quiet), None);
    }

    fn laptop(mode: Mode, source: Source) -> Power {
        Power { mode: Some(mode), high_available: true, chip: Chip::Large { capped_case: false }, source: Some(source), ..Power::default() }
    }

    #[test]
    fn high_power_on_the_recommended_adapter_holds_nothing_back() {
        let p = laptop(Mode::High, Source::Adapter { watts: Some(96), draining: false });
        assert_eq!(p.issues(), []);
        assert_eq!(p.summary(), "High Power Mode, 96 W adapter");
        let p = Power { chip: Chip::Large { capped_case: true }, ..p };
        assert_eq!(p.issues(), [], "a larger adapter would not help the 14-inch case");
    }

    #[test]
    fn a_small_adapter_is_named_without_promising_a_larger_one_fixes_it() {
        let neutral = laptop(Mode::High, Source::Adapter { watts: Some(90), draining: false });
        assert_eq!(neutral.issues(), [Issue::SmallAdapter { watts: 90, capped_case: false }]);
        assert_eq!(neutral.issues()[0].describe(), "The 90 W adapter is under the 96 W recommended for a Max or Ultra chip.");
        let capped = Power { chip: Chip::Large { capped_case: true }, ..neutral };
        let words = capped.issues()[0].describe();
        assert!(words.contains("under the 96 W recommended for this Mac"), "{words}");
        assert!(words.contains("14-inch MacBook Pro limit its GPU whatever the adapter"), "{words}");
        assert!(!capped.issues()[0].is_clock(), "input is capped near 97 W, so a few watts short is no low clock");
    }

    #[test]
    fn a_busy_process_is_named_and_does_not_count_as_a_lower_clock() {
        let p = Power { busy: Some("bun".into()), ..laptop(Mode::High, Source::Adapter { watts: Some(96), draining: false }) };
        assert_eq!(p.issues(), [Issue::Busy("bun".into())]);
        assert_eq!(p.issues()[0].describe(), "bun is using a lot of CPU, which takes power from the GPU; closing it can speed up generation.");
        assert!(!p.issues()[0].is_clock());
    }

    #[test]
    fn what_holds_the_gpu_back_is_named() {
        let p = laptop(Mode::Automatic, Source::Adapter { watts: Some(90), draining: true });
        assert_eq!(p.issues(), [Issue::NotHighPower, Issue::Draining, Issue::SmallAdapter { watts: 90, capped_case: false }]);
        let p = Power { low_power: true, thermal: Thermal::Serious, ..laptop(Mode::Automatic, Source::Battery) };
        assert_eq!(p.issues(), [Issue::LowPower, Issue::Battery, Issue::Hot(Thermal::Serious)]);
        assert_eq!(p.summary(), "Low Power Mode, on battery, hot");
    }

    #[test]
    fn automatic_is_fine_where_there_is_no_high_power_mode_and_small_adapters_suit_small_chips() {
        let p = Power { high_available: false, chip: Chip::Other, ..laptop(Mode::Automatic, Source::Adapter { watts: Some(70), draining: false }) };
        assert_eq!(p.issues(), []);
    }

    #[test]
    fn a_mac_pmset_says_nothing_about_shows_nothing() {
        assert_eq!(Power::default().issues(), []);
        assert_eq!(Power::default().summary(), "");
    }
}
