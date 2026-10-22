//! fcitx5's kimpanel panel protocol (`org.kde.impanel2`).
//!
//! # Why kimpanel, and not the input-context properties
//!
//! fcitx5 does not publish the caret rectangle as a D-Bus property. The
//! input-context objects only carry *methods* (`SetCursorRect`, called by the
//! application's own IM module) and a client-side UI signal; neither is
//! visible to a third party. The one interface designed for an external
//! candidate window is kimpanel: fcitx5's `libkimpanel.so` hands the focused
//! context's candidates and spot rectangle to whichever process owns the
//! well-known name `org.kde.impanel`.
//!
//! The relevant calls, all methods fcitx5 invokes on us at
//! `/org/kde/impanel` with interface `org.kde.impanel2`:
//!
//! * `SetLookupTable(labels, texts, attrs, hasPrev, hasNext, cursor, layout)`
//! * `SetSpotRect(x, y, w, h)`
//!
//! We implement only `SetSpotRect`, not the `SetRelativeSpotRect*` variants.
//! fcitx5 introspects the panel and, when the relative variants are absent,
//! always sends plain `SetSpotRect`. The coordinates are then whatever the
//! client handed its IM module, which for wayland clients is relative to the
//! toplevel — the same value the relative variant would carry, minus a
//! coordinate-space distinction we cannot resolve without the compositor's
//! help.
//!
//! # Availability
//!
//! fcitx5's kimpanel addon watches the name `org.kde.impanel`. Owning it makes
//! the addon available, and its `UIPriority=50` (against classicui's default
//! 0) makes fcitx5 switch `CurrentUI` to `kimpanel` on the next availability
//! check. `PanelCreated2` is the explicit "I am here" nudge; the name
//! ownership alone is sufficient, but emitting the signal also asks fcitx5 to
//! publish its status properties.
//!
//! # Threading
//!
//! Everything here runs inside the tokio runtime on its own thread. Results
//! are pushed through the same [`crate::ipc`] channel the wayland loop already
//! drains, so the panel never touches wayland or the loop state directly.

use std::sync::{Arc, Mutex};

use crate::fcitx5::events::LookupTable;
use crate::ipc::{DaemonToWayland, Sender};
use crate::niri_ipc::NiriClient;
use crate::state::IcId;
use crate::wayland::text_input::CaretRect;
use crate::Result;

/// The synthetic input-context id kimpanel messages are filed under.
///
/// kimpanel is a single globally-focused panel: fcitx5 sends us the UI of
/// whichever context is current, without naming it. A fixed id keeps the
/// existing per-IC `State` API usable unchanged.
pub const PANEL_IC: &str = "/kimpanel";

/// The well-known name fcitx5's kimpanel addon watches.
pub const PANEL_BUS_NAME: &str = "org.kde.impanel";
/// The object path fcitx5 calls methods on.
pub const PANEL_PATH: &str = "/org/kde/impanel";
/// The interface carrying `SetLookupTable` and `SetSpotRect`.
pub const PANEL_INTERFACE: &str = "org.kde.impanel2";

/// Convert kimpanel's parallel label/text arrays into a [`LookupTable`].
///
/// fcitx5 prepends `auxDown` (the page indicator, e.g. `1/3`) as an entry with
/// an empty label, and increments the cursor index to account for it. Both are
/// undone here so the renderer sees candidates only.
pub fn lookup_from_kimpanel(labels: &[String], texts: &[String], cursor: i32) -> LookupTable {
    let mut candidates = texts.to_vec();
    let mut labels = labels.to_vec();
    let mut aux_down = None;
    let mut cursor = cursor.max(0) as u32;

    if labels.first().is_some_and(String::is_empty)
        && candidates
            .first()
            .is_some_and(|t| looks_like_page_indicator(t))
    {
        aux_down = Some(candidates.remove(0));
        labels.remove(0);
        cursor = cursor.saturating_sub(1);
    }

    labels.resize(candidates.len(), String::new());
    fill_missing_labels(&mut labels);

    LookupTable {
        candidates,
        labels,
        cursor_pos: cursor,
        // kimpanel does not report a page size; the renderer only needs the
        // candidate list and the highlight index.
        page_size: 0,
        aux_up: None,
        aux_down,
        preedit: None,
    }
}

/// Whether `s` looks like fcitx5's `current/total` paging indicator.
///
/// Used to tell an auxDown entry (which carries an empty label) apart from a
/// candidate whose own label happens to be empty, e.g. when the user turned
/// candidate labels off.
fn looks_like_page_indicator(s: &str) -> bool {
    let Some((current, total)) = s.split_once('/') else {
        return false;
    };
    !current.is_empty()
        && !total.is_empty()
        && current.bytes().all(|b| b.is_ascii_digit())
        && total.bytes().all(|b| b.is_ascii_digit())
}

/// Number candidates ourselves when fcitx5 sent no labels at all.
///
/// The common case has fcitx5 supplying "1.", "2.", … already; this only
/// covers panels that omit the array so the popup still shows positions.
fn fill_missing_labels(labels: &mut [String]) {
    if labels.iter().all(String::is_empty) {
        for (idx, label) in labels.iter_mut().enumerate() {
            *label = format!("{}.", idx + 1);
        }
    }
}

fn caret_from_spot(x: i32, y: i32, w: i32, h: i32) -> CaretRect {
    CaretRect {
        x,
        y,
        width: w,
        height: h,
    }
}

/// The panel object fcitx5 calls into.
pub struct Panel {
    tx: Sender<DaemonToWayland>,
    ic: IcId,
    /// Wayland clients report the caret to the compositor, not to us, so ask
    /// niri for it and keep kimpanel's spot as the fallback.
    niri: Mutex<NiriClient>,
}

impl Panel {
    pub fn new(tx: Sender<DaemonToWayland>) -> Self {
        Self {
            tx,
            ic: Arc::from(PANEL_IC),
            niri: Mutex::new(NiriClient::from_env()),
        }
    }

    fn niri_caret(&self) -> Option<CaretRect> {
        self.niri.lock().ok()?.caret()
    }

    fn send_caret(&self, rect: CaretRect) {
        self.send(DaemonToWayland::Caret {
            ic: self.ic.clone(),
            rect,
        });
    }

    fn send(&self, msg: DaemonToWayland) {
        if let Err(e) = self.tx.send(msg) {
            tracing::warn!(error = %e, "kimpanel: wayland loop is gone");
        }
    }
}

#[zbus::interface(name = "org.kde.impanel2")]
impl Panel {
    /// fcitx5's candidate list (and optional auxDown strip) for the focused
    /// input context.
    #[zbus(name = "SetLookupTable")]
    #[allow(clippy::too_many_arguments)]
    fn set_lookup_table(
        &self,
        labels: Vec<String>,
        texts: Vec<String>,
        _attrs: Vec<String>,
        _has_prev: bool,
        _has_next: bool,
        cursor: i32,
        _layout: i32,
    ) {
        let table = lookup_from_kimpanel(&labels, &texts, cursor);
        tracing::debug!(
            candidates = table.candidates.len(),
            cursor = table.cursor_pos,
            aux_down = ?table.aux_down,
            "kimpanel: SetLookupTable"
        );
        // A panel message always refers to the context that currently owns
        // focus, so make sure it is the latest one before the table lands.
        self.send(DaemonToWayland::FocusIn(self.ic.clone()));
        // Refresh the caret alongside the table: the window may have moved
        // without the client sending a new cursor rectangle.
        if let Some(rect) = self.niri_caret() {
            self.send_caret(rect);
        }
        self.send(DaemonToWayland::LookupTable {
            ic: self.ic.clone(),
            table,
        });
    }

    /// The caret rectangle the popup should hang from.
    #[zbus(name = "SetSpotRect")]
    fn set_spot_rect(&self, x: i32, y: i32, w: i32, h: i32) {
        tracing::debug!(x, y, w, h, "kimpanel: SetSpotRect");
        self.send(DaemonToWayland::FocusIn(self.ic.clone()));
        let kimpanel = caret_from_spot(x, y, w, h);
        let rect = self.niri_caret().unwrap_or(kimpanel);
        self.send_caret(rect);
    }
}

/// Publish the panel and serve it until the process exits.
///
/// The returned `Connection` must stay alive; `serve` parks on a never-ready
/// future rather than dropping it.
pub async fn publish(tx: Sender<DaemonToWayland>) -> Result<()> {
    let panel = Panel::new(tx);

    // Owning `org.kde.impanel` disables fcitx5's own candidate window, so only
    // take it over if niri can tell us where the caret is. With stock niri
    // every Wayland client would report a zero caret, our popup would hide,
    // and the user would lose the candidate window entirely. Leaving the name
    // free lets fcitx5's classic UI draw at the caret instead.
    if !panel
        .niri
        .lock()
        .map(|mut c| c.supports_text_input_rect())
        .unwrap_or(false)
    {
        tracing::warn!(
            "niri does not report text input rectangles; \
             leaving the candidate window to fcitx5 instead of publishing kimpanel"
        );
        return Ok(());
    }

    let conn = zbus::connection::Builder::session()?
        .name(PANEL_BUS_NAME)?
        .serve_at(PANEL_PATH, panel)?
        .build()
        .await?;

    conn.emit_signal(
        None::<()>,
        PANEL_PATH,
        PANEL_INTERFACE,
        "PanelCreated2",
        &(),
    )
    .await?;

    tracing::info!(
        name = PANEL_BUS_NAME,
        path = PANEL_PATH,
        "kimpanel panel published"
    );

    std::future::pending::<()>().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn candidates_map_straight_through() {
        let t = lookup_from_kimpanel(&s(&["1.", "2."]), &s(&["你", "泥"]), 1);
        assert_eq!(t.candidates, s(&["你", "泥"]));
        assert_eq!(t.labels, s(&["1.", "2."]));
        assert_eq!(t.cursor_pos, 1);
        assert!(t.aux_down.is_none());
    }

    #[test]
    fn aux_down_is_split_off_and_cursor_shifted() {
        let t = lookup_from_kimpanel(&s(&["", "1.", "2."]), &s(&["1/3", "你", "泥"]), 2);
        assert_eq!(t.aux_down.as_deref(), Some("1/3"));
        assert_eq!(t.candidates, s(&["你", "泥"]));
        assert_eq!(t.labels, s(&["1.", "2."]));
        assert_eq!(
            t.cursor_pos, 1,
            "the auxDown entry occupies index 0, so the cursor shifts down"
        );
    }

    #[test]
    fn missing_labels_are_numbered() {
        let t = lookup_from_kimpanel(&s(&["", ""]), &s(&["你", "泥"]), 0);
        assert_eq!(t.labels, s(&["1.", "2."]));
    }

    #[test]
    fn labels_are_padded_to_the_candidate_count() {
        let t = lookup_from_kimpanel(&s(&["1."]), &s(&["你", "泥"]), 0);
        assert_eq!(t.labels, s(&["1.", ""]));
    }

    #[test]
    fn empty_lookup_is_preserved() {
        let t = lookup_from_kimpanel(&[], &[], -1);
        assert!(t.candidates.is_empty());
        assert!(t.labels.is_empty());
        assert_eq!(t.cursor_pos, 0, "a negative cursor clamps to zero");
        assert!(t.aux_down.is_none());
    }

    #[test]
    fn aux_down_alone_is_not_mistaken_for_a_candidate() {
        let t = lookup_from_kimpanel(&s(&[""]), &s(&["1/9"]), 0);
        assert_eq!(t.aux_down.as_deref(), Some("1/9"));
        assert!(t.candidates.is_empty());
        assert!(t.labels.is_empty());
    }

    #[test]
    fn spot_rect_keeps_all_four_fields() {
        let r = caret_from_spot(10, 20, 2, 18);
        assert_eq!((r.x, r.y, r.width, r.height), (10, 20, 2, 18));
    }
}
