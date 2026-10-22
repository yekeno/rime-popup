//! Internal representation of an fcitx5 candidate list.
//!
//! The kimpanel D-Bus call carries three parallel arrays (labels, texts,
//! attributes); [`crate::fcitx5::kimpanel`] folds them into this type so the
//! rest of the daemon never reasons about the wire shape.

use serde::Deserialize;

/// The candidate list plus everything the popup needs to render it.
#[derive(Debug, Default, Clone, Deserialize, PartialEq)]
pub struct LookupTable {
    /// Candidate strings in display order.
    pub candidates: Vec<String>,
    /// Short position labels ("1.", "2.", …) parallel to `candidates`.
    ///
    /// fcitx5 supplies these and lets the user configure the style, so they
    /// are rendered verbatim rather than regenerated from the index.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Index of the highlighted candidate within `candidates`.
    #[serde(default)]
    pub cursor_pos: u32,
    /// Candidates per page, used for the page indicator.
    #[serde(default)]
    pub page_size: u32,
    #[serde(default)]
    pub aux_up: Option<String>,
    #[serde(default)]
    pub aux_down: Option<String>,
    #[serde(default)]
    pub preedit: Option<String>,
}

impl LookupTable {
    /// Highlighted index, clamped into range. Returns `None` when there are no
    /// candidates, so callers never have to guard against an empty list.
    pub fn highlighted(&self) -> Option<usize> {
        if self.candidates.is_empty() {
            return None;
        }
        Some(
            self.cursor_pos
                .min(self.candidates.len().saturating_sub(1) as u32) as usize,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlighted_is_none_when_empty() {
        let l = LookupTable::default();
        assert_eq!(l.highlighted(), None);
    }

    #[test]
    fn highlighted_clamps_past_end() {
        let l = LookupTable {
            candidates: vec!["a".into(), "b".into()],
            cursor_pos: 99,
            ..Default::default()
        };
        assert_eq!(l.highlighted(), Some(1), "must clamp, never index OOB");
    }

    #[test]
    fn aux_down_is_optional() {
        let l = LookupTable {
            candidates: vec!["你".into()],
            ..Default::default()
        };
        assert_eq!(l.aux_down, None);
    }
}
