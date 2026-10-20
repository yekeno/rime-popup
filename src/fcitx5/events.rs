//! Map fcitx5 DBus payloads to internal types.
//!
//! fcitx5 signals candidate changes as a set of properties on the focused
//! input context. We fold those into a single `LookupTable` so the rest of the
//! daemon never has to reason about the DBus shape.

use serde::Deserialize;

/// The candidate list plus everything the popup needs to render it.
#[derive(Debug, Default, Clone, Deserialize, PartialEq)]
pub struct LookupTable {
    /// Candidate strings in display order.
    pub candidates: Vec<String>,
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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PageUpdated {
    pub cursor: u32,
    pub page: u32,
}

/// Build a `LookupTable` from the fcitx5 property bag.
///
/// fcitx5 sets `hasLookupTable` independently of whether candidates are
/// present; when it is false there is nothing to draw, so we return `None`
/// rather than an empty frame that would render a blank popup.
#[allow(clippy::too_many_arguments)]
pub fn extract_lookup(
    preedit: Option<String>,
    has_lookup_table: bool,
    candidates: Vec<String>,
    cursor_pos: u32,
    page_size: u32,
    aux_up: Option<String>,
    aux_down: Option<String>,
) -> Option<LookupTable> {
    if !has_lookup_table {
        return None;
    }
    Some(LookupTable {
        candidates,
        cursor_pos,
        page_size,
        aux_up,
        aux_down,
        preedit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_lookup_when_flag_off() {
        let r = extract_lookup(None, false, vec!["你".into()], 0, 1, None, None);
        assert!(r.is_none());
    }

    #[test]
    fn lookup_carries_all_fields() {
        let r = extract_lookup(
            Some("ni".into()),
            true,
            vec!["你".into(), "泥".into()],
            1,
            9,
            None,
            Some("1/3".into()),
        )
        .expect("lookup");
        assert_eq!(r.candidates.len(), 2);
        assert_eq!(r.cursor_pos, 1);
        assert_eq!(r.aux_down.as_deref(), Some("1/3"));
        assert_eq!(r.preedit.as_deref(), Some("ni"));
    }

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
    fn empty_candidates_still_produce_a_table_when_flag_set() {
        // fcitx5 can set hasLookupTable with a transient empty list; we keep
        // it so the popup can hide itself rather than flickering on the next
        // signal.
        let r = extract_lookup(None, true, vec![], 0, 0, None, None).expect("table");
        assert!(r.candidates.is_empty());
        assert_eq!(r.highlighted(), None);
    }
}
