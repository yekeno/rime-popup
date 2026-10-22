use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::errors::{Error, Result};

fn default_general() -> GeneralConfig {
    GeneralConfig { log: "info".into() }
}

fn default_render() -> RenderConfig {
    RenderConfig {
        font_family: "DejaVu Sans Mono".into(),
        font_fallback_cjk: "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".into(),
        font_size: 14,
        line_padding: 8,
        candidate_gap: 12,
        label_gap: 6,
        bg: "#1e1e1e".into(),
        highlight_bg: "#3a3a3a".into(),
        text: "#e6e6e6".into(),
        border: "#666666".into(),
        border_width: 1,
        vertical_offset: 4,
    }
}

fn default_filter() -> FilterConfig {
    FilterConfig {
        ignore_app_ids: vec!["emacs".into()],
        show_preedit: true,
        show_aux_up: false,
        show_aux_down: true,
    }
}

fn default_wayland() -> WaylandConfig {
    WaylandConfig {
        output_name: String::new(),
        layer: "overlay".into(),
        anchor: "top-left".into(),
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub render: RenderConfig,
    #[serde(default)]
    pub filter: FilterConfig,
    #[serde(default)]
    pub wayland: WaylandConfig,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct GeneralConfig {
    pub log: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        default_general()
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct RenderConfig {
    pub font_family: String,
    pub font_fallback_cjk: String,
    pub font_size: u32,
    pub line_padding: u32,
    pub candidate_gap: u32,
    /// Space between a candidate's position label and its text.
    pub label_gap: u32,
    /// Popup background. Must be distinct from `text`: the plan's original
    /// `clear_bg` filled with the text colour because no such field existed.
    pub bg: String,
    pub highlight_bg: String,
    pub text: String,
    pub border: String,
    pub border_width: u32,
    pub vertical_offset: u32,
}

impl Default for RenderConfig {
    fn default() -> Self {
        default_render()
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct FilterConfig {
    pub ignore_app_ids: Vec<String>,
    pub show_preedit: bool,
    pub show_aux_up: bool,
    pub show_aux_down: bool,
}

impl Default for FilterConfig {
    fn default() -> Self {
        default_filter()
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct WaylandConfig {
    pub output_name: String,
    pub layer: String,
    pub anchor: String,
}

impl Default for WaylandConfig {
    fn default() -> Self {
        default_wayland()
    }
}

impl Config {
    pub fn default_built() -> Self {
        Config {
            general: default_general(),
            render: default_render(),
            filter: default_filter(),
            wayland: default_wayland(),
        }
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        let parsed: Config = toml::from_str(&raw).map_err(|source| Error::Toml {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(parsed)
    }

    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var_os("HOME").expect("HOME unset");
                PathBuf::from(home).join(".config")
            });
        base.join("rime-popup").join("config.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_consistent() {
        let c = Config::default_built();
        assert_eq!(c.general.log, "info");
        assert!(c.render.font_size >= 8);
        assert!(c.filter.ignore_app_ids.contains(&"emacs".into()));
    }

    #[test]
    fn rejects_unknown_fields() {
        let cfg = r#"
            [general]
            log = "info"
            [nonsense]
            what = 1
        "#;
        let err = toml::from_str::<Config>(cfg).unwrap_err();
        assert!(format!("{err}").contains("nonsense"));
    }

    #[test]
    fn read_empty_section_uses_defaults() {
        let cfg = r#"
            [general]
            log = "debug"
        "#;
        let c: Config = toml::from_str(cfg).expect("valid");
        assert_eq!(c.general.log, "debug");
        assert_eq!(c.render.font_size, 14);
    }

    #[test]
    fn partial_filter_table_inherits_defaults() {
        let cfg = r#"
            [filter]
            show_preedit = false
        "#;
        let c: Config = toml::from_str(cfg).expect("valid");
        assert_eq!(c.filter.ignore_app_ids, vec!["emacs".to_string()]);
        assert!(c.filter.show_aux_down);
        assert!(!c.filter.show_preedit);
        // show_aux_up defaulted to false; show_preedit overridden to false
    }

    #[test]
    fn partial_render_table_inherits_defaults() {
        let cfg = r#"
            [render]
            font_size = 20
        "#;
        let c: Config = toml::from_str(cfg).expect("valid");
        assert_eq!(c.render.font_size, 20);
        assert_eq!(c.render.font_family, "DejaVu Sans Mono");
        assert_eq!(c.render.line_padding, 8);
    }
}
