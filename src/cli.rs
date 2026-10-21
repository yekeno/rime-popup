//! Command-line surface.
//!
//! Parsing lives here rather than in `main.rs` so the binary target and the
//! test suite share one definition of the flags, and so the flag names can be
//! tested without spawning a process.

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "rime-popup", about = "fcitx5 candidate popup for niri")]
pub struct Cli {
    /// Config file to read. Defaults to the XDG config path; a missing file at
    /// that path is not an error, it falls back to built-in defaults.
    #[arg(long, short, env = "RIME_POPUP_CONFIG")]
    pub config: Option<std::path::PathBuf>,

    /// Log level, overriding `[general].log`. RUST_LOG still wins if set.
    #[arg(long)]
    pub log: Option<String>,

    /// Run in foreground (do not fork).
    #[arg(long, default_value_t = false)]
    pub foreground: bool,

    /// Suppress structured log fields.
    #[arg(long, default_value_t = false)]
    pub quiet: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn parses_both_flags() {
        let c =
            Cli::try_parse_from(["rime-popup", "--log", "debug", "--foreground"]).expect("parses");
        assert_eq!(c.log.as_deref(), Some("debug"));
        assert!(c.foreground);
    }

    #[test]
    fn defaults_are_conservative() {
        let c = Cli::try_parse_from(["rime-popup"]).expect("parses");
        assert!(c.config.is_none());
        assert!(c.log.is_none());
        assert!(!c.foreground);
        assert!(!c.quiet);
    }

    #[test]
    fn short_config_flag_takes_a_path() {
        let c = Cli::try_parse_from(["rime-popup", "-c", "/tmp/x.toml"]).expect("parses");
        assert_eq!(
            c.config.as_deref(),
            Some(std::path::Path::new("/tmp/x.toml"))
        );
    }

    #[test]
    fn config_env_var_is_read() {
        // `env` attribute is declarative; asserting the command definition
        // carries it keeps the behaviour from silently regressing to
        // "RIME_POPUP_CONFIG is ignored".
        let cmd = Cli::command();
        let arg = cmd
            .get_arguments()
            .find(|a| a.get_id() == "config")
            .expect("config arg is registered");
        assert_eq!(
            arg.get_env(),
            Some(std::ffi::OsStr::new("RIME_POPUP_CONFIG"))
        );
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(Cli::try_parse_from(["rime-popup", "--nope"]).is_err());
    }

    #[test]
    fn missing_config_value_is_rejected() {
        assert!(Cli::try_parse_from(["rime-popup", "--config"]).is_err());
    }
}
