//! Compose the IMState + caret_rect into a single `PopupFrame`.
//!
//! Rendering the frame itself is in `crate::render::paint`; this module
//! decides whether to show, where to show, and what to draw.

use tiny_skia::Pixmap;

use crate::config::Config;
use crate::render::layout::{measure, PopupMetrics};
use crate::render::paint;
use crate::render::text::FontFace;
use crate::state::State;
use crate::wayland::text_input::LatestCaret;

/// One frame of the popup surface, ready to upload into the wl_shm pool.
#[derive(Debug)]
pub struct PopupFrame {
    pub metrics: PopupMetrics,
    pub canvas: Pixmap,
    /// True when the contents changed since the last frame, so the caller
    /// can decide whether to commit the wayland surface.
    pub need_draw: bool,
}

/// Decide whether the popup should be visible, and if so paint it.
///
/// Returns `None` when there is no focused input context, no caret rect yet,
/// or the focus does not currently carry a lookup table. Allocation failures
/// during `make_canvas` are also folded into `None` because the caller has no
/// useful recovery beyond skipping this frame.
pub fn decide_and_paint(
    st: &State,
    caret: &LatestCaret,
    cfg: &Config,
    font: Option<&FontFace>,
) -> Option<PopupFrame> {
    let lk = st.focused_lookup()?;
    let caret_rect = caret.rect?;
    // A zero-sized rectangle cannot anchor anything. Hiding is better than
    // drawing at the output origin, which is what an all-zero caret means.
    if caret_rect.width <= 0 || caret_rect.height <= 0 {
        return None;
    }
    let total = lk.candidates.len();
    if total == 0 {
        return None;
    }
    let metrics = measure(&lk.labels, &lk.candidates, &cfg.render);
    let mut canvas = paint::make_canvas(metrics).ok()?;
    paint::clear_bg(&mut canvas, &cfg.render);
    let cursor = lk.cursor_pos.min(total.saturating_sub(1) as u32) as usize;
    paint::draw_highlight(&mut canvas, &cfg.render, metrics, cursor, total);
    if let Some(font) = font {
        for (idx, candidate) in lk.candidates.iter().enumerate() {
            let label = lk.labels.get(idx).map_or("", String::as_str);
            crate::render::text::draw_cell(
                &mut canvas,
                font,
                metrics,
                &cfg.render,
                idx,
                label,
                candidate,
            );
        }
    }
    paint::draw_border(&mut canvas, &cfg.render);
    Some(PopupFrame {
        metrics,
        canvas,
        need_draw: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fcitx5::events::LookupTable;
    use crate::wayland::text_input::WlSurfaceId;
    use std::sync::Arc;

    #[test]
    fn returns_none_without_focus() {
        let st = State::new();
        let caret = LatestCaret::default();
        let cfg = Config::default_built();
        assert!(decide_and_paint(&st, &caret, &cfg, None).is_none());
    }

    #[test]
    fn returns_none_when_caret_unknown_even_if_lookup_present() {
        let mut st = State::new();
        let ic: Arc<str> = Arc::from("/IC/0");
        st.insert_ic(ic.clone());
        st.focus_in(&ic);
        st.set_lookup(
            &ic,
            LookupTable {
                candidates: vec!["ni".into(), "你".into()],
                labels: vec!["1.".into(), "2.".into()],
                cursor_pos: 0,
                page_size: 9,
                aux_up: None,
                aux_down: None,
                preedit: Some("ni".into()),
            },
        );
        // No `focus_enter` so `caret.rect` stays None.
        let caret = LatestCaret::default();
        assert!(decide_and_paint(&st, &caret, &Config::default_built(), None).is_none());
    }

    #[test]
    fn produces_canvas_with_focus_and_caret() {
        let mut st = State::new();
        let ic: Arc<str> = Arc::from("/IC/0");
        st.insert_ic(ic.clone());
        st.focus_in(&ic);
        st.set_lookup(
            &ic,
            LookupTable {
                candidates: vec!["ni".into(), "你".into()],
                labels: vec!["1.".into(), "2.".into()],
                cursor_pos: 0,
                page_size: 9,
                aux_up: None,
                aux_down: None,
                preedit: Some("ni".into()),
            },
        );
        let mut caret = LatestCaret::default();
        caret.focus_enter(WlSurfaceId(1));
        caret.cursor_rect(10, 10, 1, 16);
        let f = decide_and_paint(&st, &caret, &Config::default_built(), None).expect("frame");
        assert!(f.canvas.width() > 0);
        assert!(f.canvas.height() > 0);
    }

    #[test]
    fn zero_sized_caret_hides_the_popup() {
        let mut st = State::new();
        let ic: Arc<str> = Arc::from("/IC/0");
        st.insert_ic(ic.clone());
        st.focus_in(&ic);
        st.set_lookup(
            &ic,
            LookupTable {
                candidates: vec!["你".into()],
                labels: vec!["1.".into()],
                ..Default::default()
            },
        );
        let mut caret = LatestCaret::default();
        caret.focus_enter(WlSurfaceId(1));
        caret.cursor_rect(0, 0, 0, 0);
        assert!(decide_and_paint(&st, &caret, &Config::default_built(), None).is_none());
    }

    #[test]
    fn returns_none_when_lookup_has_no_candidates() {
        let mut st = State::new();
        let ic: Arc<str> = Arc::from("/IC/0");
        st.insert_ic(ic.clone());
        st.focus_in(&ic);
        st.set_lookup(
            &ic,
            LookupTable {
                candidates: vec![],
                labels: vec![],
                cursor_pos: 0,
                page_size: 0,
                aux_up: None,
                aux_down: None,
                preedit: None,
            },
        );
        let mut caret = LatestCaret::default();
        caret.focus_enter(WlSurfaceId(1));
        caret.cursor_rect(10, 10, 1, 16);
        assert!(decide_and_paint(&st, &caret, &Config::default_built(), None).is_none());
    }
}
