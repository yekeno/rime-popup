use std::process::ExitCode;

use clap::Parser;

use rime_popup::cli::Cli;
use rime_popup::config::Config;
use rime_popup::Result;

fn main() -> ExitCode {
    match real_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rime-popup: {e:#?}");
            ExitCode::FAILURE
        }
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let cfg_path = cli.config.clone().unwrap_or_else(Config::default_path);
    let cfg = if cfg_path.exists() {
        Config::load_from_path(&cfg_path)?
    } else {
        Config::default_built()
    };
    init_tracing(cfg.general.log.as_str());
    tracing::info!(config_path = %cfg_path.display(), "rime-popup starting");

    rime_popup::runtime::run(cfg)?;
    Ok(())
}

fn init_tracing(default_level: &str) {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_level))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().with_env_filter(filter).try_init();
}

#[test]
fn cli_parses_full() {
    let c = Cli::try_parse_from(["rime-popup", "--log", "debug", "--foreground"]).expect("parses");
    assert!(c.foreground);
}

#[test]
fn cli_accepts_config_override() {
    let c = Cli::try_parse_from(["rime-popup", "--config", "/tmp/rp.toml"]).expect("parses");
    assert_eq!(c.config, Some("/tmp/rp.toml".into()));
}

#[test]
fn cli_rejects_unknown_flag() {
    assert!(Cli::try_parse_from(["rime-popup", "--nope"]).is_err());
}
