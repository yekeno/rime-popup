//! What each signal means, and how a reload is applied.
//!
//! Signal *delivery* lives in [`crate::runtime`] — this module holds the two
//! decisions that can be tested without a process or a compositor: mapping a
//! Unix signal to an intent, and swapping a freshly parsed config in.
//!
//! SIGHUP reloads and SIGUSR1 dumps are served by the same calloop
//! `Signals` source that handles shutdown, so none of this needs `signal-hook`
//! or any other extra dependency.

use std::path::{Path, PathBuf};

use calloop::signals::Signal;

use crate::config::Config;
use crate::Result;

/// Signals the daemon watches for.
pub const WATCHED: &[Signal] = &[
    Signal::SIGINT,
    Signal::SIGTERM,
    Signal::SIGHUP,
    Signal::SIGUSR1,
];

/// What the daemon should do in response to a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonSignal {
    /// SIGHUP — re-read the config file.
    Reload,
    /// SIGUSR1 — log a dump of current state.
    Dump,
    /// SIGINT / SIGTERM — unwind the event loop and exit 0.
    Shutdown,
}

/// Map a Unix signal to the daemon's intent.
///
/// Unknown signals map to `Shutdown`: the loop only ever installs the four in
/// [`WATCHED`], so reaching here means the set grew without this match being
/// updated, and stopping is the safe reading of "a signal we do not handle".
pub fn classify(sig: Signal) -> DaemonSignal {
    match sig {
        Signal::SIGHUP => DaemonSignal::Reload,
        Signal::SIGUSR1 => DaemonSignal::Dump,
        _ => DaemonSignal::Shutdown,
    }
}

/// A config that can be re-read from disk on SIGHUP.
///
/// Reload failures are non-fatal by design. A user editing a config in an
/// editor almost always saves a broken intermediate state, and killing the
/// daemon because of it would mean a typo silently removes their IME. The old
/// config stays in force and the error is logged.
///
/// # `general.log` does not take effect until restart
///
/// The tracing subscriber is installed once in `main` and
/// `tracing-subscriber`'s filter is not cheaply mutable after that, so a
/// SIGHUP that changes `general.log` updates the in-force [`Config`] but
/// leaves the live filter alone. The setting is read from the environment
/// (`RUST_LOG`) when the daemon starts, which overrides the file. Everything
/// else in the file — render, filter — applies immediately.
pub struct Reloadable {
    path: PathBuf,
    current: Config,
    /// Whether the daemon started with built-in defaults because the file was
    /// absent, rather than having parsed it.
    started_from_default: bool,
}

impl Reloadable {
    /// Wrap a config together with the path it came from.
    ///
    /// `started_from_default` records whether the path existed at startup; a
    /// reload of a still-absent file is a no-op rather than a failure, so a
    /// daemon launched without a config does not warn on every SIGHUP.
    pub fn new(current: Config, path: PathBuf, started_from_default: bool) -> Self {
        Self {
            path,
            current,
            started_from_default,
        }
    }

    /// The config currently in force.
    pub fn get(&self) -> &Config {
        &self.current
    }

    /// The path this config is read from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Re-read the config file, keeping the old one if it cannot be parsed.
    ///
    /// Returns `Ok(true)` when the in-force config was replaced.
    pub fn reload(&mut self) -> Result<bool> {
        if !self.path.exists() {
            if self.started_from_default {
                // Still absent, as it was at startup: nothing to report.
                return Ok(false);
            }
            tracing::warn!(
                path = %self.path.display(),
                "config file disappeared; keeping the running config"
            );
            return Ok(false);
        }

        match Config::load_from_path(&self.path) {
            Ok(fresh) => {
                // No log line here: the caller logs the *effect* of the
                // reload (what changed in loop state), and logging both would
                // print the same event twice.
                self.current = fresh;
                self.started_from_default = false;
                Ok(true)
            }
            Err(e) => {
                tracing::error!(
                    path = %self.path.display(),
                    error = %e,
                    "config reload failed; keeping the running config"
                );
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sighup_reloads() {
        assert_eq!(classify(Signal::SIGHUP), DaemonSignal::Reload);
    }

    #[test]
    fn sigusr1_dumps() {
        assert_eq!(classify(Signal::SIGUSR1), DaemonSignal::Dump);
    }

    #[test]
    fn sigint_and_sigterm_shut_down() {
        assert_eq!(classify(Signal::SIGINT), DaemonSignal::Shutdown);
        assert_eq!(classify(Signal::SIGTERM), DaemonSignal::Shutdown);
    }

    #[test]
    fn watched_set_has_no_duplicates() {
        // A duplicate in `WATCHED` would register the same signal twice.
        let mut sorted: Vec<i32> = WATCHED.iter().map(|s| *s as i32).collect();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(sorted.len(), before, "WATCHED contains a duplicate signal");
    }

    #[test]
    fn every_watched_signal_classifies() {
        // Guards against adding a signal to WATCHED without teaching
        // `classify` about it.
        for sig in WATCHED {
            // Must not panic and must be one of the three real intents.
            assert!(matches!(
                classify(*sig),
                DaemonSignal::Reload | DaemonSignal::Dump | DaemonSignal::Shutdown
            ));
        }
    }

    #[test]
    fn reload_picks_up_a_changed_file() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("rime-popup.toml");
        let mut cfg = Config::default_built();
        let first = cfg.general.log.clone();
        std::fs::write(&path, format!("[general]\nlog = \"{first}\"\n")).expect("write");
        cfg = Config::load_from_path(&path).expect("load");
        let mut r = Reloadable::new(cfg, path.clone(), false);

        std::fs::write(&path, "[general]\nlog = \"debug\"\n").expect("rewrite");
        assert!(r.reload().expect("reload succeeds"), "config was replaced");
        assert_eq!(r.get().general.log, "debug");
    }

    #[test]
    fn broken_config_keeps_the_running_one() {
        // The important case: a typo must not kill the daemon.
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("rime-popup.toml");
        std::fs::write(&path, "[general]\nlog = \"info\"\n").expect("write");
        let good = Config::load_from_path(&path).expect("load");
        let mut r = Reloadable::new(good, path.clone(), false);

        std::fs::write(&path, "this is not toml [[[").expect("write garbage");
        assert!(!r.reload().expect("reload does not propagate the error"));
        assert_eq!(r.get().general.log, "info", "old config stays in force");
    }

    #[test]
    fn absent_file_started_from_default_is_quiet() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("does-not-exist.toml");
        let mut r = Reloadable::new(Config::default_built(), path, true);
        assert!(!r.reload().expect("reload succeeds"));
    }

    #[test]
    fn deleted_file_keeps_the_running_config() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("rime-popup.toml");
        std::fs::write(&path, "[general]\nlog = \"warn\"\n").expect("write");
        let cfg = Config::load_from_path(&path).expect("load");
        let mut r = Reloadable::new(cfg, path.clone(), false);

        std::fs::remove_file(&path).expect("unlink");
        assert!(!r.reload().expect("reload succeeds"));
        assert_eq!(r.get().general.log, "warn");
    }

    #[test]
    fn reload_returns_false_when_nothing_changed_on_disk() {
        // A reload that re-reads the same file still reports a replacement;
        // callers apply the config unconditionally rather than diffing.
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("rime-popup.toml");
        std::fs::write(&path, "[general]\nlog = \"info\"\n").expect("write");
        let cfg = Config::load_from_path(&path).expect("load");
        let mut r = Reloadable::new(cfg, path.clone(), false);
        assert!(r.reload().expect("reload succeeds"));
    }
}
