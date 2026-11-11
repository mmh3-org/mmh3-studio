//! Image sizes the studio offers, and the rule a custom size must follow.

/// How many pixels a picture has: a quick look, or the model's full size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Quality {
    #[default]
    Draft,
    Final,
}

impl Quality {
    pub const ALL: [Quality; 2] = [Quality::Draft, Quality::Final];

    pub fn label(self) -> &'static str {
        match self {
            Quality::Draft => "Draft",
            Quality::Final => "Final",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Quality::Draft => "4 MP",
            Quality::Final => "16 MP",
        }
    }
}

/// A shape, at the model's documented ~16 MP sizes for Final and the same shape at ~4 MP for
/// Draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aspect {
    pub name: &'static str,
    pub draft: (u32, u32),
    pub full: (u32, u32),
}

impl Aspect {
    pub fn size(self, q: Quality) -> (u32, u32) {
        match q {
            Quality::Draft => self.draft,
            Quality::Final => self.full,
        }
    }
}

pub const ASPECTS: [Aspect; 8] = [
    Aspect { name: "1:1", draft: (2048, 2048), full: (4096, 4096) },
    // Halving 4736×3552 gives 1776, which is not a multiple of 32.
    Aspect { name: "4:3", draft: (2304, 1728), full: (4736, 3552) },
    Aspect { name: "3:4", draft: (1728, 2304), full: (3552, 4736) },
    Aspect { name: "3:2", draft: (2496, 1664), full: (5024, 3360) },
    Aspect { name: "2:3", draft: (1664, 2496), full: (3360, 5024) },
    Aspect { name: "16:9", draft: (2720, 1536), full: (5472, 3072) },
    Aspect { name: "9:16", draft: (1536, 2720), full: (3072, 5472) },
    Aspect { name: "21:9", draft: (3136, 1344), full: (6272, 2688) },
];

pub const MULTIPLE: u32 = 32;
pub const MIN_SIDE: u32 = 256;
/// The largest side the preview can decode and draw.
pub const MAX_SIDE: u32 = 8192;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SizeError {
    NotANumber,
    NotMultiple,
    OutOfRange,
}

impl std::fmt::Display for SizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SizeError::NotANumber => write!(f, "Width and height must be whole numbers."),
            SizeError::NotMultiple => write!(f, "Width and height must be multiples of {MULTIPLE}."),
            SizeError::OutOfRange => write!(f, "Each side must be between {MIN_SIDE} and {MAX_SIDE} pixels."),
        }
    }
}

pub fn side(text: &str) -> Result<u32, SizeError> {
    let n: u32 = text.trim().parse().map_err(|_| SizeError::NotANumber)?;
    if !(MIN_SIDE..=MAX_SIDE).contains(&n) {
        return Err(SizeError::OutOfRange);
    }
    if !n.is_multiple_of(MULTIPLE) {
        return Err(SizeError::NotMultiple);
    }
    Ok(n)
}

pub fn custom(width: &str, height: &str) -> Result<(u32, u32), SizeError> {
    Ok((side(width)?, side(height)?))
}

pub fn megapixels(width: u32, height: u32) -> f64 {
    f64::from(width) * f64::from(height) / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_offered_size_is_a_valid_custom_size() {
        for a in ASPECTS {
            for q in Quality::ALL {
                let (w, h) = a.size(q);
                assert_eq!(custom(&w.to_string(), &h.to_string()), Ok((w, h)), "{} {q:?}", a.name);
            }
        }
    }

    #[test]
    fn custom_sizes_need_multiples_of_32_within_range() {
        assert_eq!(custom("1024", "768"), Ok((1024, 768)));
        assert_eq!(custom(" 1024 ", "768"), Ok((1024, 768)));
        assert_eq!(custom("1000", "768"), Err(SizeError::NotMultiple));
        assert_eq!(custom("1024", "abc"), Err(SizeError::NotANumber));
        assert_eq!(custom("", "768"), Err(SizeError::NotANumber));
        assert_eq!(custom("128", "768"), Err(SizeError::OutOfRange));
        assert_eq!(custom("8224", "768"), Err(SizeError::OutOfRange));
    }

    #[test]
    fn final_is_sixteen_megapixels_and_draft_four_in_the_named_shape() {
        for a in ASPECTS {
            let (n, d) = a.name.split_once(':').unwrap();
            let ratio = n.parse::<f64>().unwrap() / d.parse::<f64>().unwrap();
            for (q, mp) in [(Quality::Draft, 3.9..4.3), (Quality::Final, 16.0..17.0)] {
                let (w, h) = a.size(q);
                assert!(mp.contains(&megapixels(w, h)), "{} {q:?}: {w}×{h} is {} MP", a.name, megapixels(w, h));
                let off = (f64::from(w) / f64::from(h) / ratio - 1.0).abs();
                assert!(off < 0.01, "{} {q:?}: {w}×{h} is {off} off its shape", a.name);
            }
        }
    }
}
