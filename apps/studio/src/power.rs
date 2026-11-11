//! Whether macOS lets the GPU run at full speed. A Mac outside High Power Mode, on battery, on
//! a small adapter or held back for heat keeps the GPU at a low clock, and a generation takes
//! two or three times as long.
//!
//! Everything is read from `pmset` and `NSProcessInfo`, neither of which needs root.

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
    /// A Max or Ultra chip, which needs Apple's 140 W adapter at full load.
    pub large_chip: bool,
    pub low_power: bool,
    pub thermal: Thermal,
    pub source: Option<Source>,
}

/// The smallest adapter a laptop with a Max chip runs its GPU at full clock on.
pub const FULL_SPEED_WATTS: u32 = 140;

/// What holds the GPU back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Issue {
    LowPower,
    NotHighPower,
    Battery,
    Draining,
    SmallAdapter(u32),
    Hot(Thermal),
}

impl Issue {
    pub fn describe(self) -> String {
        match self {
            Issue::LowPower => "Low Power Mode is on.".into(),
            Issue::NotHighPower => "Energy Mode is Automatic, not High Power.".into(),
            Issue::Battery => "The Mac is running on battery.".into(),
            Issue::Draining => "The battery is draining although the adapter is plugged in.".into(),
            Issue::SmallAdapter(w) => format!("The {w} W adapter is smaller than the {FULL_SPEED_WATTS} W this Mac needs at full load."),
            Issue::Hot(Thermal::Critical) => "macOS reports a critical thermal state.".into(),
            Issue::Hot(_) => "macOS reports a serious thermal state.".into(),
        }
    }

    /// Fixed by switching Energy Mode, which the copied command does.
    pub fn is_mode(self) -> bool {
        matches!(self, Issue::LowPower | Issue::NotHighPower)
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
                if let Some(w) = watts.filter(|w| self.large_chip && *w < FULL_SPEED_WATTS) {
                    issues.push(Issue::SmallAdapter(w));
                }
            }
            None => {}
        }
        if matches!(self.thermal, Thermal::Serious | Thermal::Critical) {
            issues.push(Issue::Hot(self.thermal));
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
    fn only_max_and_ultra_chips_want_the_large_adapter() {
        assert!(parse_large_chip("Apple M5 Max\n"));
        assert!(parse_large_chip("Apple M2 Ultra"));
        assert!(!parse_large_chip("Apple M5 Pro"));
        assert!(!parse_large_chip(""));
    }

    fn laptop(mode: Mode, source: Source) -> Power {
        Power { mode: Some(mode), high_available: true, large_chip: true, source: Some(source), ..Power::default() }
    }

    #[test]
    fn high_power_on_the_full_adapter_holds_nothing_back() {
        let p = laptop(Mode::High, Source::Adapter { watts: Some(140), draining: false });
        assert_eq!(p.issues(), []);
        assert_eq!(p.summary(), "High Power Mode, 140 W adapter");
    }

    #[test]
    fn what_holds_the_gpu_back_is_named() {
        let p = laptop(Mode::Automatic, Source::Adapter { watts: Some(90), draining: true });
        assert_eq!(p.issues(), [Issue::NotHighPower, Issue::Draining, Issue::SmallAdapter(90)]);
        let p = Power { low_power: true, thermal: Thermal::Serious, ..laptop(Mode::Automatic, Source::Battery) };
        assert_eq!(p.issues(), [Issue::LowPower, Issue::Battery, Issue::Hot(Thermal::Serious)]);
        assert_eq!(p.summary(), "Low Power Mode, on battery, hot");
    }

    #[test]
    fn automatic_is_fine_where_there_is_no_high_power_mode_and_small_adapters_suit_small_chips() {
        let p = Power { high_available: false, large_chip: false, ..laptop(Mode::Automatic, Source::Adapter { watts: Some(70), draining: false }) };
        assert_eq!(p.issues(), []);
    }

    #[test]
    fn a_mac_pmset_says_nothing_about_shows_nothing() {
        assert_eq!(Power::default().issues(), []);
        assert_eq!(Power::default().summary(), "");
    }
}
