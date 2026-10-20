#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("wayland protocol error: {0}")]
    Wayland(#[from] wayland_client::backend::WaylandError),

    #[error("wayland backend connect error: {0}")]
    WaylandConnect(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("dbus error: {0}")]
    Dbus(#[from] zbus::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("toml parse error at {path}: {source}")]
    Toml {
        path: std::path::PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("config invalid: {0}")]
    Config(String),

    #[error("fcitx5 ic error: {0}")]
    Fcitx5(String),

    #[error("render error: {0}")]
    Render(String),

    #[error("internal: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_error_display() {
        let e = Error::Config("bad font size".into());
        assert_eq!(format!("{e}"), "config invalid: bad font size");
    }

    #[test]
    fn result_ok_is_inferred() {
        let r: Result<i32> = Ok(7);
        assert!(matches!(r, Ok(7)));
    }
}
