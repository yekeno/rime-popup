//! Per-input-context state and the global "latest focus" pointer.
//!
//! fcitx5 has one input context per focused text input. Only the most
//! recently focused one drives the popup, so `State` keeps per-IC data plus a
//! single `latest_focus` key.

use std::collections::HashMap;
use std::sync::Arc;

use crate::fcitx5::events::LookupTable;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct IMState {
    pub has_focus: bool,
    pub preedit: Option<String>,
    pub lookup: Option<LookupTable>,
}

/// Dbus object path identifying an fcitx5 input context.
pub type IcId = Arc<str>;

#[derive(Debug, Default)]
pub struct State {
    ics: HashMap<IcId, IMState>,
    latest_focus: Option<IcId>,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_ic(&mut self, ic: IcId) {
        self.ics.entry(ic).or_default();
    }

    pub fn focus_in(&mut self, ic: &IcId) {
        if let Some(s) = self.ics.get_mut(ic) {
            s.has_focus = true;
        }
        self.latest_focus = Some(ic.clone());
    }

    /// Drop focus from `ic`. Only clears `latest_focus` when `ic` is the
    /// current one — losing focus to another context must not blank the popup
    /// for the context that just gained it.
    pub fn focus_out(&mut self, ic: &IcId) {
        if let Some(s) = self.ics.get_mut(ic) {
            s.has_focus = false;
            s.lookup = None;
            s.preedit = None;
        }
        if self.latest_focus.as_deref() == Some(ic.as_ref()) {
            self.latest_focus = None;
        }
    }

    pub fn set_lookup(&mut self, ic: &IcId, lk: LookupTable) {
        if let Some(s) = self.ics.get_mut(ic) {
            s.lookup = Some(lk);
        }
    }

    pub fn set_preedit(&mut self, ic: &IcId, preedit: Option<String>) {
        if let Some(s) = self.ics.get_mut(ic) {
            s.preedit = preedit;
        }
    }

    pub fn remove_ic(&mut self, ic: &IcId) {
        self.ics.remove(ic);
        if self.latest_focus.as_deref() == Some(ic.as_ref()) {
            self.latest_focus = None;
        }
    }

    pub fn latest_focus(&self) -> Option<&IcId> {
        self.latest_focus.as_ref()
    }

    pub fn ic_count(&self) -> usize {
        self.ics.len()
    }

    /// The lookup table for the focused context, if any.
    ///
    /// Requires `has_focus` as well as `latest_focus`: a context can be the
    /// latest to focus and still have lost focus, and we must not draw then.
    pub fn focused_lookup(&self) -> Option<&LookupTable> {
        let ic = self.latest_focus.as_ref()?;
        let s = self.ics.get(ic)?;
        if !s.has_focus {
            return None;
        }
        s.lookup.as_ref()
    }

    pub fn focused_preedit(&self) -> Option<&str> {
        let ic = self.latest_focus.as_ref()?;
        let s = self.ics.get(ic)?;
        s.preedit.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ic() -> IcId {
        Arc::from("/InputContext/42")
    }

    fn table() -> LookupTable {
        LookupTable {
            candidates: vec!["你".into()],
            ..Default::default()
        }
    }

    #[test]
    fn focus_in_records_latest() {
        let mut st = State::new();
        let i = ic();
        st.insert_ic(i.clone());
        st.focus_in(&i);
        assert_eq!(st.latest_focus().map(|s| s.as_ref()), Some(i.as_ref()));
    }

    #[test]
    fn focus_out_clears_latest_match() {
        let mut st = State::new();
        let i = ic();
        st.insert_ic(i.clone());
        st.focus_in(&i);
        st.focus_out(&i);
        assert!(st.latest_focus().is_none());
        assert!(st.focused_lookup().is_none());
    }

    #[test]
    fn remove_ic_clears_focus_match() {
        let mut st = State::new();
        let i = ic();
        st.insert_ic(i.clone());
        st.focus_in(&i);
        st.remove_ic(&i);
        assert!(st.latest_focus().is_none());
        assert_eq!(st.ic_count(), 0);
    }

    #[test]
    fn focused_lookup_requires_both_focus_and_table() {
        let mut st = State::new();
        let i = ic();
        st.insert_ic(i.clone());
        st.set_lookup(&i, table());

        // Table set but no focus yet: must not render.
        assert!(st.focused_lookup().is_none());

        st.focus_in(&i);
        assert!(st.focused_lookup().is_some());

        // Losing focus must hide it again.
        st.focus_out(&i);
        assert!(st.focused_lookup().is_none());
    }

    #[test]
    fn latest_wins_when_focus_moves_between_contexts() {
        let mut st = State::new();
        let a: IcId = Arc::from("/InputContext/1");
        let b: IcId = Arc::from("/InputContext/2");
        st.insert_ic(a.clone());
        st.insert_ic(b.clone());
        st.set_lookup(&a, table());
        st.focus_in(&a);
        st.focus_in(&b);

        assert_eq!(st.latest_focus().map(|s| s.as_ref()), Some(b.as_ref()));
        // `a` still has a table but is no longer focused.
        assert!(st.focused_lookup().is_none());
    }

    #[test]
    fn focus_out_clears_stale_candidate_data() {
        let mut st = State::new();
        let i = ic();
        st.insert_ic(i.clone());
        st.focus_in(&i);
        st.set_lookup(&i, table());
        st.set_preedit(&i, Some("ni".into()));
        st.focus_out(&i);

        st.focus_in(&i);
        assert!(
            st.focused_lookup().is_none(),
            "a re-entered context must not resurrect its old candidates"
        );
        assert!(st.focused_preedit().is_none());
    }

    #[test]
    fn focus_in_on_unknown_ic_does_not_panic() {
        let mut st = State::new();
        let ghost: IcId = Arc::from("/InputContext/ghost");
        st.focus_in(&ghost);
        assert_eq!(st.latest_focus().map(|s| s.as_ref()), Some(ghost.as_ref()));
        assert!(st.focused_lookup().is_none());
    }
}
