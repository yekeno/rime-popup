//! Wrap `zwp_text_input_v3` events into a clean `LatestCaret` value.
//!
//! The protocol emits `enter` and `leave` around the focused text input. We
//! collapse those into the single "most recent caret" the popup positions
//! itself against.
//!
//! # The caret rectangle does not come from wayland
//!
//! It is tempting to read [`CaretRect`] off a `zwp_text_input_v3` `done`
//! event, but that event carries only a `serial`; there is no cursor field in
//! the protocol at all. `set_cursor_rectangle` runs the *other* direction —
//! it is a request an input method sends to the compositor to say where the
//! caret is. An input method is therefore never told where the caret is over
//! wayland, and cannot ask.
//!
//! The rectangle that positions the popup comes from fcitx5's own DBus input
//! context instead — see [`crate::fcitx5::events::extract_caret`]. What this
//! module contributes is the *focus* half: knowing which surface is live, so
//! a rectangle arriving from a context we are no longer tracking can be
//! discarded rather than drawn at a stale position.

/// Opaque identifier for a `WlSurface`, taken from its wire id.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WlSurfaceId(pub u32);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CaretRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Default, Clone)]
pub struct LatestCaret {
    pub focus: Option<WlSurfaceId>,
    pub rect: Option<CaretRect>,
}

impl LatestCaret {
    pub fn focus_enter(&mut self, id: WlSurfaceId) {
        self.focus = Some(id);
        self.rect = None;
    }

    /// Leaving a surface only clears state if it is the focused one. A
    /// `leave` for a background surface must not disturb the active caret.
    pub fn focus_leave(&mut self, id: WlSurfaceId) {
        if self.focus == Some(id) {
            self.focus = None;
            self.rect = None;
        }
    }

    /// Record a cursor rect. Ignored while unfocused: a rect arriving without
    /// an `enter` refers to a surface we are not tracking, and adopting it
    /// would place the popup at a stale position.
    pub fn cursor_rect(&mut self, x: i32, y: i32, w: i32, h: i32) {
        if self.focus.is_some() {
            self.rect = Some(CaretRect {
                x,
                y,
                width: w,
                height: h,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_then_leave_clears() {
        let mut c = LatestCaret::default();
        c.focus_enter(WlSurfaceId(7));
        c.cursor_rect(10, 20, 1, 16);
        assert!(c.rect.is_some());
        c.focus_leave(WlSurfaceId(7));
        assert!(c.focus.is_none());
        assert!(c.rect.is_none());
    }

    #[test]
    fn leave_other_surface_is_noop() {
        let mut c = LatestCaret::default();
        c.focus_enter(WlSurfaceId(7));
        c.cursor_rect(10, 20, 1, 16);
        c.focus_leave(WlSurfaceId(99));
        assert_eq!(c.focus, Some(WlSurfaceId(7)));
        assert!(c.rect.is_some(), "a stray leave must not drop the caret");
    }

    #[test]
    fn cursor_rect_before_enter_is_ignored() {
        let mut c = LatestCaret::default();
        c.cursor_rect(10, 20, 1, 16);
        assert!(c.rect.is_none());
    }

    #[test]
    fn re_entering_resets_stale_rect() {
        let mut c = LatestCaret::default();
        c.focus_enter(WlSurfaceId(1));
        c.cursor_rect(5, 5, 1, 16);
        c.focus_enter(WlSurfaceId(2));
        assert!(c.rect.is_none(), "new focus must not inherit the old caret");
    }

    #[test]
    fn latest_rect_wins() {
        let mut c = LatestCaret::default();
        c.focus_enter(WlSurfaceId(1));
        c.cursor_rect(1, 1, 1, 16);
        c.cursor_rect(2, 3, 1, 20);
        let r = c.rect.expect("rect");
        assert_eq!((r.x, r.y, r.height), (2, 3, 20));
    }
}
