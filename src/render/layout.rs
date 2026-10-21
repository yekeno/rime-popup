//! Pure layout math: given candidate strings, how large is the popup and
//! where does each candidate sit? Unit-tested without any renderer.

use crate::config::RenderConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PopupMetrics {
    /// Full popup size in pixels.
    pub width: u32,
    pub height: u32,
    /// Nominal horizontal advance for a half-width glyph, used for the
    /// fallback cell width when we have no candidates to measure.
    pub char_w: u32,
    /// Width of one candidate cell, including its trailing gap.
    pub cell_w: u32,
}

/// Approximate whether `ch` renders at full em width.
///
/// We only need to separate the two families that matter here: CJK and
/// fullwidth forms (one em) versus Latin and ASCII (about half an em).
/// U+2E80 is the start of the CJK Radicals block, and everything from there
/// through the CJK blocks, Hangul, and the Halfwidth/Fullwidth forms is wide.
/// This is deliberately approximate — exact metrics need real font tables,
/// which `measure` cannot consult without loading a face.
fn is_wide(ch: char) -> bool {
    matches!(ch as u32,
        0x1100..=0x115F      // Hangul Jamo
        | 0x2E80..=0x303E    // CJK radicals, Kangxi, CJK symbols
        | 0x3041..=0x33FF    // Hiragana, Katakana, Bopomofo, CJK compat
        | 0x3400..=0x4DBF    // CJK Ext A
        | 0x4E00..=0x9FFF    // CJK Unified Ideographs
        | 0xA000..=0xA4CF    // Yi
        | 0xAC00..=0xD7A3    // Hangul syllables
        | 0xF900..=0xFAFF    // CJK compatibility ideographs
        | 0xFF00..=0xFF60    // Fullwidth forms
        | 0xFFE0..=0xFFE6    // Fullwidth signs
    )
}

/// Horizontal advance of `s` at `font_size`, in pixels.
pub fn advance(s: &str, font_size: u32) -> u32 {
    let half = font_size / 2;
    s.chars()
        .map(|ch| if is_wide(ch) { font_size } else { half })
        .sum()
}

/// Size the popup for `candidates`.
///
/// The popup is as wide as the widest candidate plus its gap, times the
/// candidate count, plus padding on both sides. An empty candidate list still
/// yields a valid, minimally-sized box so the surface can exist before the
/// first candidates arrive.
pub fn measure(candidates: &[String], cfg: &RenderConfig) -> PopupMetrics {
    let widest = candidates
        .iter()
        .map(|s| advance(s, cfg.font_size))
        .max()
        .unwrap_or(0);
    // Always reserve at least one half-width glyph so an empty list does not
    // produce a zero-width cell.
    let content = widest.max(cfg.font_size / 2);
    let cell_w = content + cfg.candidate_gap;
    let slots = candidates.len().max(1) as u32;

    PopupMetrics {
        width: cell_w * slots + 2 * cfg.line_padding,
        height: cfg.font_size + 2 * cfg.line_padding,
        char_w: cfg.font_size / 2,
        cell_w,
    }
}

/// Left edge of candidate `idx` within the popup, in pixels.
pub fn cell_x(m: &PopupMetrics, cfg: &RenderConfig, idx: usize) -> u32 {
    cfg.line_padding + m.cell_w * idx as u32
}

/// Wrap `s` into chunks of at most `max_chars` characters.
///
/// Wraps on character count, not bytes, so multi-byte CJK never splits.
/// A `max_chars` of 0 returns the whole string rather than looping forever.
pub fn split_lines(s: &str, max_chars: usize) -> Vec<String> {
    if max_chars == 0 {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut count = 0;
    for c in s.chars() {
        cur.push(c);
        count += 1;
        if count >= max_chars {
            out.push(std::mem::take(&mut cur));
            count = 0;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> RenderConfig {
        crate::config::Config::default_built().render
    }

    #[test]
    fn measure_one_candidate() {
        let m = measure(&["你".into()], &cfg());
        assert!(m.width > 0);
        assert!(m.height >= cfg().font_size);
    }

    #[test]
    fn measure_handles_empty() {
        let m = measure(&[], &cfg());
        assert!(
            m.width >= 2 * cfg().line_padding,
            "empty list still needs padding on both sides"
        );
        assert!(m.cell_w > 0, "an empty list must still reserve a cell");
    }

    #[test]
    fn cjk_is_wider_than_ascii() {
        let c = cfg();
        let cjk = measure(&["你你你".into()], &c);
        let ascii = measure(&["abc".into()], &c);
        assert!(
            cjk.width > ascii.width,
            "three full-width glyphs must exceed three half-width ones"
        );
    }

    #[test]
    fn advance_counts_chars_not_bytes() {
        // Each CJK char is 3 UTF-8 bytes but one full em.
        assert_eq!(advance("你好", 14), 28);
        assert_eq!(advance("ab", 14), 14);
    }

    #[test]
    fn width_grows_with_candidate_count() {
        let c = cfg();
        let one = measure(&["你".into()], &c);
        let three = measure(&["你".into(), "好".into(), "吗".into()], &c);
        assert!(
            three.width > one.width,
            "each extra candidate must add a cell, not share one"
        );
    }

    #[test]
    fn cells_tile_without_overlap() {
        let c = cfg();
        let m = measure(&["你".into(), "好".into()], &c);
        let x0 = cell_x(&m, &c, 0);
        let x1 = cell_x(&m, &c, 1);
        assert_eq!(x1 - x0, m.cell_w);
        assert!(
            x1 + m.cell_w + c.line_padding <= m.width + c.line_padding,
            "last cell must stay inside the popup"
        );
    }

    #[test]
    fn split_lines_wraps() {
        let parts = split_lines("你好世界再见", 2);
        assert_eq!(parts, vec!["你好", "世界", "再见"]);
    }

    #[test]
    fn split_lines_keeps_remainder() {
        assert_eq!(split_lines("你好世", 2), vec!["你好", "世"]);
    }

    #[test]
    fn split_lines_zero_max_returns_whole() {
        assert_eq!(split_lines("你好", 0), vec!["你好"]);
    }

    #[test]
    fn split_lines_empty_input() {
        assert!(split_lines("", 4).is_empty());
    }
}
