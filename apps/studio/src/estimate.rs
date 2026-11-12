//! How long a picture takes: a fixed part, six steps whose cost grows faster than the pixels
//! (attention spans every token), and a decode that grows with them. The shape comes from
//! measurements; the studio's own finished generations scale it to the Mac it runs on.

use crate::api::{IMAGE_STEPS, Timings};

/// Seconds, from a picture's megapixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Model {
    /// Everything besides loading, the steps and the decode: reading the prompt, writing the PNG.
    pub fixed: f64,
    /// One step: `a·MP + b·MP²`.
    pub step: [f64; 2],
    /// The decode: `c + d·MP`.
    pub decode: [f64; 2],
    /// Loading the models into a server that does not hold them.
    pub load: f64,
}

/// A 14-inch M5 Max: 2048² at ≈ 4.4 s a step and 1.4 s to decode, 30 s in all; 4096² at
/// ≈ 32 s a step and 5 s to decode, 3.5 min in all; 6 s to load the models cold.
pub const MEASURED: Model = Model { fixed: 3.0, step: [0.754, 0.0705], decode: [0.2, 0.286], load: 6.0 };

/// How many of the latest finished generations the fit looks at, so it follows a Mac whose
/// power settings change.
const RECENT: usize = 24;
/// The least time loading that says a run read its models, rather than finding them held: the
/// server times its loads on every run, and a warm one takes a few hundredths of a second.
const COLD_LOAD: f64 = 1.0;

impl Model {
    pub fn step_at(&self, mp: f64) -> f64 {
        self.step[0] * mp + self.step[1] * mp * mp
    }

    pub fn decode_at(&self, mp: f64) -> f64 {
        self.decode[0] + self.decode[1] * mp
    }

    pub fn steps_at(&self, mp: f64) -> f64 {
        f64::from(IMAGE_STEPS) * self.step_at(mp)
    }

    /// A generation on a server that holds the models.
    pub fn warm(&self, mp: f64) -> f64 {
        self.fixed + self.steps_at(mp) + self.decode_at(mp)
    }

    pub fn seconds(&self, mp: f64, cold: bool) -> f64 {
        self.warm(mp) + if cold { self.load } else { 0.0 }
    }

    fn scaled(&self, fixed: f64, step: f64, decode: f64, load: f64) -> Model {
        Model { fixed: self.fixed * fixed, step: self.step.map(|v| v * step), decode: self.decode.map(|v| v * decode), load: self.load * load }
    }
}

/// A finished generation: its megapixels, how long it took and, from a server that says,
/// where the time went.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub mp: f64,
    pub seconds: f64,
    pub timings: Option<Timings>,
}

fn median(values: impl IntoIterator<Item = f64>) -> Option<f64> {
    let mut v: Vec<f64> = values.into_iter().filter(|x| x.is_finite() && *x > 0.0).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let mid = v.len() / 2;
    Some(if v.len().is_multiple_of(2) { f64::midpoint(v[mid - 1], v[mid]) } else { v[mid] })
}

/// Scales [`MEASURED`] to `samples`, newest first. Generations with stage timings scale each
/// part on its own, the load from the ones that read their models; without any, the total
/// scales the whole. The median keeps one cold start or one slow run from moving it.
pub fn fit(samples: impl IntoIterator<Item = Sample>) -> Model {
    let m = MEASURED;
    let samples: Vec<Sample> = samples.into_iter().filter(|s| s.mp > 0.0 && s.seconds > 0.0).take(RECENT).collect();
    let timed: Vec<(f64, f64, &Timings)> =
        samples.iter().filter_map(|s| s.timings.as_ref().filter(|t| !t.steps.is_empty()).map(|t| (s.mp, s.seconds, t))).collect();
    if !timed.is_empty() {
        let step = median(timed.iter().map(|(mp, _, t)| t.steps.iter().sum::<f64>() / t.steps.len() as f64 / m.step_at(*mp))).unwrap_or(1.0);
        let decode = median(timed.iter().filter_map(|(mp, _, t)| t.decode.map(|d| d / m.decode_at(*mp)))).unwrap_or(step);
        let fixed = median(timed.iter().map(|(mp, total, t)| {
            let decoded = t.decode.unwrap_or_else(|| m.decode_at(*mp) * decode);
            (total - t.load().unwrap_or(0.0) - t.steps.iter().sum::<f64>() - decoded) / m.fixed
        }))
        .unwrap_or(step);
        let load = median(timed.iter().filter_map(|(_, _, t)| t.load().filter(|l| *l >= COLD_LOAD).map(|l| l / m.load))).unwrap_or(step);
        return m.scaled(fixed, step, decode, load);
    }
    match median(samples.iter().map(|s| s.seconds / m.warm(s.mp))) {
        Some(k) => m.scaled(k, k, k, k),
        None => m,
    }
}

/// A time as the studio says it ahead: whole seconds when short, then fives, then half and
/// whole minutes, so it never claims more precision than a guess has.
pub fn approx(seconds: f64) -> String {
    let s = seconds.max(1.0);
    if s < 10.0 {
        format!("{} s", s.round())
    } else if s < 55.0 {
        format!("{} s", (s / 5.0).round() * 5.0)
    } else if s < 600.0 {
        let half = (s / 30.0).round() / 2.0;
        if half.fract() == 0.0 { format!("{half} min") } else { format!("{half:.1} min") }
    } else {
        format!("{} min", (s / 60.0).round())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sizes::megapixels;

    fn mp(w: u32, h: u32) -> f64 {
        megapixels(w, h)
    }

    #[test]
    fn the_defaults_match_the_measurements() {
        let draft = mp(2048, 2048);
        let full = mp(4096, 4096);
        assert!((MEASURED.step_at(draft) - 4.4).abs() < 0.1, "{}", MEASURED.step_at(draft));
        assert!((30.0..35.0).contains(&MEASURED.step_at(full)), "{}", MEASURED.step_at(full));
        assert!((MEASURED.decode_at(draft) - 1.4).abs() < 0.1);
        assert!((MEASURED.decode_at(full) - 5.0).abs() < 0.2);
        assert_eq!(approx(MEASURED.warm(draft)), "30 s");
        assert_eq!(approx(MEASURED.warm(full)), "3.5 min");
        assert_eq!(approx(MEASURED.seconds(draft, true)), "35 s");
    }

    #[test]
    fn times_round_to_what_a_guess_can_say() {
        assert_eq!(approx(0.2), "1 s");
        assert_eq!(approx(7.4), "7 s");
        assert_eq!(approx(31.0), "30 s");
        assert_eq!(approx(53.0), "55 s");
        assert_eq!(approx(62.0), "1 min");
        assert_eq!(approx(100.0), "1.5 min");
        assert_eq!(approx(241.8), "4 min");
        assert_eq!(approx(900.0), "15 min");
    }

    #[test]
    fn no_history_keeps_the_measurements() {
        assert_eq!(fit([]), MEASURED);
    }

    #[test]
    fn totals_alone_scale_the_whole_model() {
        let draft = mp(2048, 2048);
        let twice = MEASURED.warm(draft) * 2.0;
        let fitted = fit([Sample { mp: draft, seconds: twice, timings: None }]);
        assert!((fitted.warm(draft) - twice).abs() < 1e-9);
        assert!((fitted.warm(mp(4096, 4096)) - 2.0 * MEASURED.warm(mp(4096, 4096))).abs() < 1e-9);
    }

    #[test]
    fn one_cold_start_does_not_move_the_median() {
        let draft = mp(2048, 2048);
        let warm = MEASURED.warm(draft);
        let samples = [warm + 40.0, warm, warm].map(|seconds| Sample { mp: draft, seconds, timings: None });
        assert!((fit(samples).warm(draft) - warm).abs() < 1e-9);
    }

    #[test]
    fn stage_timings_fit_each_part() {
        let full = mp(4096, 4096);
        let timings = Timings {
            load_text: Some(4.0),
            encode: Some(1.0),
            load_dit: Some(8.0),
            steps: vec![40.0; 6],
            decode_read: Some(0.0),
            decode: Some(10.0),
            write: Some(0.5),
        };
        let total = 12.0 + 1.0 + 240.0 + 10.0 + 0.5 + 4.5;
        let fitted = fit([Sample { mp: full, seconds: total, timings: Some(timings) }]);
        assert!((fitted.step_at(full) - 40.0).abs() < 1e-9);
        assert!((fitted.decode_at(full) - 10.0).abs() < 1e-9);
        assert!((fitted.fixed - 6.0).abs() < 1e-9);
        assert!((fitted.load - 12.0).abs() < 1e-9);
        assert!((fitted.warm(full) - (total - 12.0)).abs() < 1e-9);
    }

    #[test]
    fn the_load_is_fitted_from_cold_starts_alone() {
        let draft = mp(2048, 2048);
        let run = |load_text, load_dit| Timings {
            load_text,
            encode: Some(0.5),
            load_dit: Some(load_dit),
            steps: vec![MEASURED.step_at(draft); 6],
            decode_read: Some(0.0004),
            decode: Some(MEASURED.decode_at(draft)),
            write: Some(0.2),
        };
        let sample = |t: Timings| Sample { mp: draft, seconds: MEASURED.warm(draft) + t.load().unwrap_or(0.0), timings: Some(t) };
        let cold = sample(run(Some(1.89), 10.53));
        let warm = || sample(run(None, 0.0548));
        let fitted = fit([warm(), warm(), cold, warm()]);
        assert!((fitted.load - (1.89 + 10.53 + 0.0004)).abs() < 1e-9, "{}", fitted.load);
        assert!((fitted.fixed - MEASURED.fixed).abs() < 1e-9, "a warm run's own load is not fixed time");
        let fitted = fit([warm(), warm()]);
        assert!((fitted.load - MEASURED.load).abs() < 1e-9, "without a cold start the measured load stays");
    }
}
