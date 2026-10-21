//! Wayland registry discovery.
//!
//! The first task in any wayland client is walking `wl_registry` and binding
//! the globals we need. `Needed` accumulates the globals as the compositor
//! advertises them, and reports when the set is complete enough to start.
//!
//! Keys are registry *names* (the server-assigned opaque u32), not interface
//! strings, because names are what `wl_registry.bind` requires. The interface
//! name is kept alongside the version so callers can ask "did I see
//! `zwlr_layer_shell_v1`, and at what version?".

use std::collections::HashMap;

/// Every global this daemon must bind before it can render a popup.
pub const REQUIRED: [&str; 5] = [
    "wl_compositor",
    "wl_shm",
    "wl_seat",
    "zwp_text_input_manager_v3",
    "zwlr_layer_shell_v1",
];

#[derive(Debug, Default, Clone)]
pub struct Needed {
    /// registry name -> (interface, advertised version)
    found: HashMap<u32, (String, u32)>,
}

impl Needed {
    pub fn is_satisfied(&self) -> bool {
        REQUIRED
            .iter()
            .all(|iface| self.version_of(iface).is_some())
    }

    /// Version of the named global, if that interface has been advertised.
    pub fn version_of(&self, interface: &str) -> Option<u32> {
        self.found
            .values()
            .find(|(iface, _)| iface == interface)
            .map(|(_, version)| *version)
    }

    /// Registry name to pass to `wl_registry.bind` for `interface`.
    pub fn name_of(&self, interface: &str) -> Option<u32> {
        self.found
            .iter()
            .find(|(_, (iface, _))| iface == interface)
            .map(|(name, _)| *name)
    }

    pub fn len(&self) -> usize {
        self.found.len()
    }

    pub fn is_empty(&self) -> bool {
        self.found.is_empty()
    }
}

/// Record one `wl_registry.global` advertisement.
///
/// Returns `true` if the global is one we care about, `false` if it is
/// irrelevant (in which case `globals` is left untouched and the caller
/// should not bind it).
pub fn record(globals: &mut Needed, name: u32, interface: &str, version: u32) -> bool {
    if !REQUIRED.contains(&interface) {
        return false;
    }
    globals.found.insert(name, (interface.to_owned(), version));
    true
}

/// Handle one `wl_registry.global_remove`.
///
/// Removes by registry name; unknown names are ignored, which is correct:
/// the compositor is allowed to remove globals we never bound.
pub fn remove(globals: &mut Needed, name: u32) {
    globals.found.remove(&name);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needed_defaults_to_unsatisfied() {
        assert!(!Needed::default().is_satisfied());
    }

    #[test]
    fn needed_satisfies_only_after_all_five() {
        let mut n = Needed::default();
        for (name, iface) in [
            (1u32, "wl_compositor"),
            (2, "wl_shm"),
            (3, "wl_seat"),
            (4, "zwp_text_input_manager_v3"),
            (5, "zwlr_layer_shell_v1"),
        ] {
            record(&mut n, name, iface, 1);
            let remaining = REQUIRED.len() - n.len();
            if remaining > 0 {
                assert!(
                    !n.is_satisfied(),
                    "must not be satisfied with {remaining} still missing"
                );
            }
        }
        assert!(n.is_satisfied());
    }

    #[test]
    fn tracks_name_and_version_per_interface() {
        let mut n = Needed::default();
        record(&mut n, 42, "wl_shm", 2);
        assert_eq!(n.version_of("wl_shm"), Some(2));
        assert_eq!(n.name_of("wl_shm"), Some(42));
        assert_eq!(n.version_of("wl_seat"), None);
    }

    #[test]
    fn remove_clears_binding() {
        let mut n = Needed::default();
        record(&mut n, 7, "wl_shm", 1);
        assert!(n.version_of("wl_shm").is_some());
        remove(&mut n, 7);
        assert!(n.version_of("wl_shm").is_none());
        assert!(!n.is_satisfied());
    }

    #[test]
    fn unknown_global_ignored() {
        let mut n = Needed::default();
        assert!(!record(&mut n, 9, "wl_drm", 1));
        assert!(n.is_empty());
        assert!(!n.is_satisfied());
    }

    #[test]
    fn remove_of_unknown_name_is_a_noop() {
        let mut n = Needed::default();
        record(&mut n, 1, "wl_shm", 1);
        remove(&mut n, 999);
        assert_eq!(n.len(), 1);
    }
}
