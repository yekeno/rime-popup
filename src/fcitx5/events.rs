//! Map fcitx5 DBus payloads to internal types.
//!
//! fcitx5 signals candidate changes as a set of properties on the focused
//! input context. We fold those into a single `LookupTable` so the rest of the
//! daemon never has to reason about the DBus shape.

use serde::Deserialize;

use crate::wayland::text_input::CaretRect;

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

/// Property carrying a complete caret rectangle, if fcitx5 provides it.
pub const CURSOR_RECT_PROP: &str = "CursorRect";
/// Property carrying the caret's top-left position.
pub const CURSOR_POS_PROP: &str = "CursorPos";
/// Property carrying the caret's size, used when no full rect is published.
pub const CURSOR_PER_CHAR_PROP: &str = "CursorPosPerChar";

/// Build a [`CaretRect`] from whatever geometry fcitx5 published.
///
/// fcitx5 has published the caret two ways across versions, and this daemon
/// does not assume either is present:
///
/// * `CursorRect` — a complete `(x, y, width, height)`. Preferred when set.
/// * `CursorPos` + `CursorPosPerChar` — the same rectangle split across two
///   `(x, y)`-ish properties. Used only as a fallback.
///
/// Returning `None` for a degenerate rectangle (non-positive width or height)
/// is deliberate: fcitx5 zeroes the geometry when a context has no caret, and
/// a zero-width rect would place the popup at the window origin rather than
/// nowhere.
pub fn extract_caret(
    cursor_rect: Option<(i32, i32, i32, i32)>,
    cursor_pos: Option<(i32, i32)>,
    per_char: Option<(i32, i32)>,
) -> Option<CaretRect> {
    if let Some((x, y, w, h)) = cursor_rect {
        // Fall through rather than return on a degenerate rect: fcitx5 zeroes
        // `CursorRect` when it is not meaningfully set, which is exactly the
        // case where the split properties may still hold a usable value.
        if let Some(rect) = viable(x, y, w, h) {
            return Some(rect);
        }
    }
    let (x, y) = cursor_pos?;
    let (w, h) = per_char?;
    viable(x, y, w, h)
}

/// Reject rectangles that cannot position a popup.
fn viable(x: i32, y: i32, w: i32, h: i32) -> Option<CaretRect> {
    if w <= 0 || h <= 0 {
        return None;
    }
    Some(CaretRect {
        x,
        y,
        width: w,
        height: h,
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

    // --- caret extraction ---------------------------------------------------

    #[test]
    fn caret_prefers_the_full_rect() {
        let c = extract_caret(Some((10, 20, 2, 16)), Some((1, 1)), Some((1, 1))).expect("caret");
        assert_eq!(
            c,
            CaretRect {
                x: 10,
                y: 20,
                width: 2,
                height: 16
            }
        );
    }

    #[test]
    fn caret_falls_back_to_pos_and_size() {
        // No full rect: compose it from the split properties.
        let c = extract_caret(None, Some((7, 9)), Some((1, 18))).expect("caret");
        assert_eq!(
            c,
            CaretRect {
                x: 7,
                y: 9,
                width: 1,
                height: 18
            }
        );
    }

    #[test]
    fn caret_is_none_when_no_geometry_published() {
        assert_eq!(extract_caret(None, None, None), None);
    }

    #[test]
    fn caret_is_none_when_size_missing() {
        // A position with no size cannot place a popup; do not invent one.
        assert_eq!(extract_caret(None, Some((3, 4)), None), None);
    }

    #[test]
    fn caret_rejects_zero_sized_rect() {
        // fcitx5 zeroes geometry when a context has no caret. Adopting it
        // would anchor the popup at the window origin instead of hiding it.
        assert_eq!(extract_caret(Some((5, 5, 0, 0)), None, None), None);
        assert_eq!(extract_caret(Some((5, 5, 0, 12)), None, None), None);
        assert_eq!(extract_caret(None, Some((5, 5)), Some((0, 12))), None);
    }

    #[test]
    fn degenerate_rect_falls_through_to_split_properties() {
        // A zeroed full rect must not shadow usable split geometry.
        let c = extract_caret(Some((0, 0, 0, 0)), Some((2, 3)), Some((1, 16))).expect("caret");
        assert_eq!(c.x, 2);
        assert_eq!(c.height, 16);
    }
}
