//! Track the set of live fcitx5 input contexts.
//!
//! fcitx5 creates one input-context object per focused text input and removes
//! it on blur. We keep the set of known object paths so the renderer can tell
//! which contexts it has state for, and so a `Removed` signal can be matched
//! to a stored path.
//!
//! Contexts are keyed by `Arc<str>` rather than `OwnedObjectPath` because
//! `OwnedObjectPath` implements neither `Ord` nor `Clone` cheaply enough to
//! sort, and because `crate::state` already keys its per-IC map on
//! `Arc<str>`. Sharing the key type means a Dbus path converts straight into
//! a state lookup with no re-parsing.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use zbus::zvariant::OwnedObjectPath;

use crate::state::IcId;

/// Convert a Dbus object path into the shared input-context key.
pub fn ic_id(path: &OwnedObjectPath) -> IcId {
    Arc::from(path.as_str())
}

#[derive(Debug, Default, Clone)]
pub struct Fcitx5Watcher {
    ics: Arc<Mutex<BTreeSet<IcId>>>,
}

impl Fcitx5Watcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&self, id: IcId) {
        // A poisoned mutex means another thread panicked while holding it. The
        // set is still structurally valid, so recovering beats propagating a
        // panic into the event loop.
        let mut set = self.ics.lock().unwrap_or_else(|e| e.into_inner());
        set.insert(id);
    }

    pub fn remove(&self, id: &IcId) {
        let mut set = self.ics.lock().unwrap_or_else(|e| e.into_inner());
        set.remove(id);
    }

    pub fn contains(&self, id: &IcId) -> bool {
        let set = self.ics.lock().unwrap_or_else(|e| e.into_inner());
        set.contains(id)
    }

    pub fn snapshot(&self) -> Vec<IcId> {
        let set = self.ics.lock().unwrap_or_else(|e| e.into_inner());
        set.iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        let set = self.ics.lock().unwrap_or_else(|e| e.into_inner());
        set.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> IcId {
        Arc::from(s)
    }

    fn path(s: &str) -> OwnedObjectPath {
        OwnedObjectPath::try_from(s).expect("valid object path")
    }

    #[test]
    fn ic_id_converts_dbus_path() {
        assert_eq!(&*ic_id(&path("/InputContext/1")), "/InputContext/1");
    }

    #[test]
    fn ic_id_rejects_invalid_path() {
        assert!(OwnedObjectPath::try_from("not-a-path").is_err());
    }

    #[test]
    fn add_remove_snapshot() {
        let w = Fcitx5Watcher::new();
        w.add(id("/InputContext/1"));
        w.add(id("/InputContext/2"));
        assert_eq!(w.snapshot().len(), 2);
        w.remove(&id("/InputContext/1"));
        assert_eq!(w.snapshot().len(), 1);
    }

    #[test]
    fn add_is_idempotent() {
        let w = Fcitx5Watcher::new();
        w.add(id("/InputContext/1"));
        w.add(id("/InputContext/1"));
        assert_eq!(w.len(), 1, "the same IC must not be tracked twice");
    }

    #[test]
    fn removing_absent_id_is_a_noop() {
        let w = Fcitx5Watcher::new();
        w.add(id("/InputContext/1"));
        w.remove(&id("/InputContext/99"));
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn contains_tracks_membership() {
        let w = Fcitx5Watcher::new();
        w.add(id("/InputContext/1"));
        assert!(w.contains(&id("/InputContext/1")));
        assert!(!w.contains(&id("/InputContext/2")));
    }

    #[test]
    fn snapshot_is_sorted_for_determinism() {
        let w = Fcitx5Watcher::new();
        w.add(id("/InputContext/30"));
        w.add(id("/InputContext/2"));
        w.add(id("/InputContext/100"));
        let names: Vec<String> = w.snapshot().iter().map(|s| s.to_string()).collect();
        assert_eq!(
            names,
            vec!["/InputContext/100", "/InputContext/2", "/InputContext/30"],
            "snapshot must be byte-sorted so rendering is reproducible"
        );
    }

    #[test]
    fn empty_by_default() {
        let w = Fcitx5Watcher::new();
        assert!(w.is_empty());
        assert!(w.snapshot().is_empty());
    }
}
