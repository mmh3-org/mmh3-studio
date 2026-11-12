//! How hard a picture works: the options of mmh3 that trade a picture's quality for time, chosen
//! as one level. High is the server's defaults; the others add only what changes anything at
//! the size asked for, since Sol-Attn leaves a short canvas dense and draft steps need sides in
//! multiples of 64.

use serde::{Deserialize, Serialize};

use crate::api::{Attention, IMAGE_STEPS, Precision, Tuning};
use crate::estimate::Run;
use crate::sizes::{self, MULTIPLE};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    #[default]
    High,
    Custom,
}

impl Effort {
    pub const ALL: [Effort; 4] = [Effort::Low, Effort::Medium, Effort::High, Effort::Custom];

    pub fn label(self) -> &'static str {
        match self {
            Effort::Low => "Low",
            Effort::Medium => "Medium",
            Effort::High => "High",
            Effort::Custom => "Custom",
        }
    }
}

/// The fewest tokens of a canvas Sol-Attn runs sparse on; a token is 32×32 pixels.
pub const SOL_MIN_TOKENS: u64 = 12_288;
/// Draft steps run on half the canvas, whose sides must still be multiples of 32.
pub const DRAFT_MULTIPLE: u32 = 2 * MULTIPLE;
/// The draft steps Low runs.
pub const LOW_DRAFT_STEPS: u32 = 2;
/// Where Sol-Attn starts unless asked otherwise: a fifth of the way through, the third step.
pub const SOL_START: f64 = 0.2;
/// Sol-Attn's routing threshold unless asked otherwise; higher is sparser.
pub const SOL_TAU: f64 = 1.3;
/// The steps, counted from 1, Custom can start Sol-Attn at; the first is the default.
pub const SOL_FROM: [u32; 3] = [3, 4, 5];

/// The `sparse-start` that starts Sol-Attn at step `step`, counted from 1. It stays a little
/// short of the step's own fraction, so mmh3's single-precision comparison cannot round it to
/// the next step.
pub fn start_fraction(step: u32) -> f64 {
    (f64::from(step.saturating_sub(1)) / f64::from(IMAGE_STEPS) * 100.0).floor() / 100.0
}

/// The step, counted from 1, that `sparse-start` `fraction` starts Sol-Attn at.
pub fn start_step(fraction: f64) -> u32 {
    (0..IMAGE_STEPS).find(|&i| f64::from(i) >= fraction * f64::from(IMAGE_STEPS)).unwrap_or(IMAGE_STEPS) + 1
}

/// What a picture is generated on: the canvas mmh3 takes its size to, and whether it edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub edit: bool,
}

/// Why a picture takes no draft steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoDraft {
    Edit,
    Side { width: u32, height: u32 },
}

impl std::fmt::Display for NoDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoDraft::Edit => write!(f, "an edit takes no draft steps"),
            NoDraft::Side { width, height } => write!(f, "draft steps need sides in multiples of {DRAFT_MULTIPLE}, and this canvas is {width} × {height}"),
        }
    }
}

impl Canvas {
    /// The canvas of a picture of `size`, generated at least at `min_area` (mmh3's default when
    /// `None`).
    pub fn new((width, height): (u32, u32), min_area: Option<u64>, edit: bool) -> Canvas {
        let (width, height) = sizes::canvas(width, height, min_area.unwrap_or(sizes::DEFAULT_MIN_AREA));
        Canvas { width, height, edit }
    }

    pub fn tokens(self) -> u64 {
        u64::from(self.width / MULTIPLE) * u64::from(self.height / MULTIPLE)
    }

    /// Whether Sol-Attn makes its full-size steps sparse.
    pub fn sparse(self) -> bool {
        self.tokens() >= SOL_MIN_TOKENS
    }

    /// Whether it can take draft steps.
    pub fn draft(self) -> Result<(), NoDraft> {
        if self.edit {
            return Err(NoDraft::Edit);
        }
        if !self.width.is_multiple_of(DRAFT_MULTIPLE) || !self.height.is_multiple_of(DRAFT_MULTIPLE) {
            return Err(NoDraft::Side { width: self.width, height: self.height });
        }
        Ok(())
    }

    /// How its six steps run with `tuning`.
    pub fn run(self, tuning: &Tuning) -> Run {
        let draft = tuning.draft_steps.unwrap_or(0).min(IMAGE_STEPS - 1);
        let sparse = match tuning.attention {
            Some(Attention::Sol) if self.sparse() => IMAGE_STEPS - draft.max(start_step(tuning.sparse_start.unwrap_or(SOL_START)) - 1).min(IMAGE_STEPS),
            _ => 0,
        };
        Run { mp: sizes::megapixels(self.width, self.height), draft, sparse }
    }
}

/// What Custom is set to. It keeps a choice that does not apply at the size asked for, and
/// leaves it out of the request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Custom {
    pub sol: bool,
    /// The step Sol-Attn starts at, counted from 1.
    pub sol_from: u32,
    /// Sol-Attn's threshold; `None` leaves the server's.
    pub tau: Option<f64>,
    pub draft_steps: u32,
    /// INT8 queries and keys for the attention scores.
    pub int8: bool,
}

impl Default for Custom {
    fn default() -> Custom {
        Custom { sol: false, sol_from: SOL_FROM[0], tau: None, draft_steps: 0, int8: false }
    }
}

impl Effort {
    /// What this level sends for a picture on `canvas`.
    pub fn tuning(self, canvas: Canvas, custom: &Custom) -> Tuning {
        let sparse = canvas.sparse();
        let draft = canvas.draft().is_ok();
        let sol = |on: bool| (on && sparse).then_some(Attention::Sol);
        match self {
            Effort::High => Tuning::default(),
            Effort::Medium => Tuning { attention: sol(true), ..Tuning::default() },
            Effort::Low => Tuning { attention: sol(true), draft_steps: draft.then_some(LOW_DRAFT_STEPS), ..Tuning::default() },
            Effort::Custom => {
                let attention = sol(custom.sol);
                Tuning {
                    attention,
                    sparse_tau: attention.and(custom.tau),
                    sparse_start: attention.and((custom.sol_from != SOL_FROM[0]).then(|| start_fraction(custom.sol_from))),
                    draft_steps: (draft && custom.draft_steps > 0).then_some(custom.draft_steps),
                    attention_precision: custom.int8.then_some(Precision::Int8Fp16),
                }
            }
        }
    }

    /// What this level trades at `canvas`, in a line.
    pub fn hint(self, canvas: Option<Canvas>) -> String {
        let Some(canvas) = canvas else {
            return match self {
                Effort::High => "The server's defaults: the most faithful picture, and the longest wait.".into(),
                _ => "Pick a valid size to see what this level changes.".into(),
            };
        };
        let short = format!("Sol-Attn needs {} tokens (32 × 32 pixels each), and this canvas has {}", grouped(SOL_MIN_TOKENS), grouped(canvas.tokens()));
        match (self, canvas.sparse(), canvas.draft()) {
            (Effort::High, ..) => "The server's defaults: the most faithful picture, and the longest wait.".into(),
            (Effort::Custom, ..) => "Only the options that change anything at this size are shown.".into(),
            (Effort::Medium, true, _) => "Sol-Attn from the third step: a little faster, but it may rewrite small lettering.".into(),
            (Effort::Medium, false, _) => format!("Same as High at this size: {short}."),
            (Effort::Low, true, Ok(())) => {
                "Two half-size draft steps and Sol-Attn: about half the time, but the same seed gives another composition and small lettering may change."
                    .into()
            }
            (Effort::Low, true, Err(why)) => format!("Sol-Attn from the third step, which may rewrite small lettering; no draft steps, since {why}."),
            (Effort::Low, false, Ok(())) => "Two half-size draft steps: faster, but the same seed gives another composition.".into(),
            (Effort::Low, false, Err(why)) => format!("Same as High at this size: {why}, and {}.", lower(&short)),
        }
    }
}

/// `n` with its thousands set apart: 12,288.
pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, d) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(d);
    }
    out
}

fn lower(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_lowercase().chain(c).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(w: u32, h: u32) -> Canvas {
        Canvas::new((w, h), None, false)
    }

    #[test]
    fn high_sends_nothing_and_a_small_canvas_gets_no_sol_attn() {
        let custom = Custom { sol: true, ..Custom::default() };
        for effort in Effort::ALL {
            assert_eq!(effort.tuning(canvas(2048, 2048), &custom).attention, None, "{effort:?}");
        }
        assert!(Effort::High.tuning(canvas(4096, 4096), &custom).is_empty());
        assert_eq!(Effort::Medium.tuning(canvas(4096, 4096), &custom), Tuning { attention: Some(Attention::Sol), ..Tuning::default() });
    }

    #[test]
    fn low_drafts_where_the_canvas_allows_and_adds_sol_attn_where_it_helps() {
        let low = |c: Canvas| Effort::Low.tuning(c, &Custom::default());
        assert_eq!(low(canvas(2048, 2048)), Tuning { draft_steps: Some(2), ..Tuning::default() });
        assert_eq!(low(canvas(4096, 4096)), Tuning { attention: Some(Attention::Sol), draft_steps: Some(2), ..Tuning::default() });
        assert_eq!(low(canvas(4736, 3552)), Tuning { attention: Some(Attention::Sol), ..Tuning::default() });
        assert_eq!(canvas(2304, 1728).draft(), Err(NoDraft::Side { width: 2368, height: 1760 }));
        assert_eq!(low(Canvas::new((2048, 2048), None, true)), Tuning::default());
    }

    #[test]
    fn custom_sends_only_what_applies() {
        let custom = Custom { sol: true, sol_from: 4, tau: Some(1.5), draft_steps: 3, int8: true };
        let big = Effort::Custom.tuning(canvas(4096, 4096), &custom);
        assert_eq!(
            big,
            Tuning {
                attention: Some(Attention::Sol),
                sparse_tau: Some(1.5),
                sparse_start: Some(0.5),
                draft_steps: Some(3),
                attention_precision: Some(Precision::Int8Fp16)
            }
        );
        let small = Effort::Custom.tuning(canvas(2304, 1728), &custom);
        assert_eq!(small, Tuning { attention_precision: Some(Precision::Int8Fp16), ..Tuning::default() });
    }

    #[test]
    fn sol_attn_starts_at_the_step_asked_for() {
        assert_eq!(start_step(SOL_START), 3);
        for step in SOL_FROM {
            assert_eq!(start_step(start_fraction(step)), step);
            // mmh3 compares in single precision.
            let first = (0..IMAGE_STEPS).find(|&i| i as f32 >= start_fraction(step) as f32 * IMAGE_STEPS as f32).unwrap();
            assert_eq!(first + 1, step);
        }
        assert_eq!(start_fraction(4), 0.5);
    }

    #[test]
    fn a_run_counts_the_draft_and_sparse_steps() {
        let sol = Tuning { attention: Some(Attention::Sol), ..Tuning::default() };
        let full = canvas(4096, 4096);
        assert_eq!((full.run(&sol).draft, full.run(&sol).sparse), (0, 4));
        assert_eq!(canvas(2048, 2048).run(&sol).sparse, 0);
        let low = Tuning { draft_steps: Some(2), ..sol };
        assert_eq!((full.run(&low).draft, full.run(&low).sparse), (2, 4));
        assert_eq!(full.run(&Tuning { sparse_start: Some(0.5), ..sol }).sparse, 3);
        assert_eq!(full.run(&Tuning { draft_steps: Some(4), sparse_start: Some(0.5), ..sol }).sparse, 2);
        assert_eq!(canvas(1024, 1024).run(&Tuning::default()).mp, sizes::megapixels(2048, 2048), "a small picture runs on the minimum area");
    }
}
