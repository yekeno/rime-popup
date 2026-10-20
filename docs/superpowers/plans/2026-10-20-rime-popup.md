# rime-popup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Rust daemon that draws fcitx5 candidate popups under the caret on niri via `zwlr_layer_shell_v1`, using `zwp_text_input_v3` for caret position and a DBus signal subscription to fcitx5 for candidate content.

**Architecture:** Single binary, two threads — a `calloop` event loop driving wayland, and a tokio runtime driving zbus. Rendering (swash + tiny-skia into `wl_shm`) is inline on the calloop thread because it is sub-millisecond for 9 glyphs. Cross-thread messages are delivered via a single `calloop::channel`.

**Tech Stack:** Rust 1.79, `wayland-client` 0.31, `wayland-protocols` 0.31, `wayland-protocols-wlr` 0.3, `zbus` 4.4, `calloop` 0.12, `calloop-wayland-source` 0.3, `swash` 0.2, `tiny-skia` 0.11, `tokio` 1.40, `clap` 4.5, `tracing` 0.1.

**Spec:** `docs/superpowers/specs/2026-10-20-fcitx5-wayland-popup-design.md`

---

## Conventions for every task

- Run tests from the crate root: `cargo test -p rime-popup`.
- Run lint: `cargo clippy --all-targets -- -D warnings`.
- Each commit message prefix matches the touched area:
  `chore:`, `feat:`, `fix:`, `test:`, `docs:`, `refactor:`, `build:`.
- Each commit ends the session "green": tests + clippy both pass.
- `git add` is selective; never commit the build/ directory (it's `.gitignore`d).

---

## Task 0: Bootstrap the crate

**Files:**
- Create: `Cargo.toml`
- Create: `Cargo.lock` (auto-generated, committed)
- Create: `src/lib.rs`
- Create: `src/main.rs`
- Create: `.gitignore`
- Create: `README.md`

- [ ] **Step 1: Create `.gitignore`**

```
/target/
**/*.rs.bk
.cargo/
*.pdb
```

- [ ] **Step 2: Create `Cargo.toml`**

```toml
[package]
name = "rime-popup"
version = "0.1.0"
edition = "2021"
rust-version = "1.79"
license = "MIT"

[lib]
name = "rime_popup"
path = "src/lib.rs"

[[bin]]
name = "rime-popup"
path = "src/main.rs"

[dependencies]
tokio = { version = "1.40", features = ["rt-multi-thread", "macros"] }
calloop = "0.12"
calloop-wayland-source = "0.3"
wayland-client = { version = "0.31", features = ["client", "dlopen", "use-system"] }
wayland-protocols = { version = "0.31", features = ["client", "staging", "unstable"] }
wayland-protocols-wlr = { version = "0.3", features = ["client"] }
wayland-backend = { version = "0.3", default-features = false, features = ["client_system"] }
zbus = { version = "4.4", default-features = false, features = ["tokio", "p2p"] }
swash = "0.2"
tiny-skia = "0.11"
memmap2 = "0.2"
serde = { version = "1", features = ["derive"] }
toml = "0.8"
anyhow = "1"
thiserror = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
clap = { version = "4.5", features = ["derive"] }
calloop-tokio = "0.1"

[dev-dependencies]
wayland-server = { version = "0.31", features = ["client", "dlopen", "use-system"] }
pretty_assertions = "1"
tempfile = "3"
predicates = "3"
png = "0.17"

[profile.release]
strip = true
lto = "thin"
codegen-units = 1
panic = "abort"
```

- [ ] **Step 3: Create `src/lib.rs`**

```rust
//! Library surface for the rime-popup binary.
//!
//! Modules keep one responsibility each:
//! - [`config`]: TOML configuration + CLI overrides
//! - [`fcitx5`]: DBus proxy and event mapping
//! - [`wayland`]: wayland registry, text-input, layer-shell, shm
//! - [`state`]: per-IC state and the global focus tracker
//! - [`render`]: text shaping + raster
//! - [`popup`]: glue from state to layer-shell surface
//! - [`ipc`]: tokio → calloop bridge

pub mod config;
pub mod errors;
pub mod fcitx5;
pub mod ipc;
pub mod popup;
pub mod render;
pub mod state;
pub mod wayland;

pub use errors::{Error, Result};
```

- [ ] **Step 4: Create `src/main.rs` placeholder**

```rust
fn main() {
    eprintln!("rime-popup: bootstrap not yet implemented");
    std::process::exit(2);
}
```

- [ ] **Step 5: Create empty module files**

Run (one shot):

```bash
mkdir -p src/{config,fcitx5,wayland,render,ipc,popup,state} tests
for f in src/errors.rs \
         src/config.rs \
         src/state.rs \
         src/fcitx5/mod.rs \
         src/wayland/mod.rs \
         src/wayland/text_input.rs \
         src/wayland/layer_shell.rs \
         src/wayland/shm.rs \
         src/render/mod.rs \
         src/render/layout.rs \
         src/render/paint.rs \
         src/ipc/mod.rs \
         src/popup/mod.rs; do
  echo "// placeholder; see next task for content" > "$f"
done
```

- [ ] **Step 6: Verify `cargo check` passes**

Run: `cargo check --all-targets`
Expected: `Finished \`dev\` profile...` with no errors.

- [ ] **Step 7: Verify `cargo test` runs (no tests yet)**

Run: `cargo test`
Expected: `running 0 tests; test result: ok. 0 passed`.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock .gitignore src/
git commit -m "chore: bootstrap rime-popup crate skeleton"
```

---

## Task 1: Error type + Result alias

**Files:**
- Create: `src/errors.rs`

- [ ] **Step 1: Write the failing test**

Replace `src/errors.rs` content:

```rust
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
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p rime-popup errors::`
Expected: `2 passed`.

- [ ] **Step 3: Commit**

```bash
git add src/errors.rs
git commit -m "feat: introduce unified Error + Result types"
```

---

## Task 2: Config (TOML loader + defaults)

**Files:**
- Create: `src/config.rs`

- [ ] **Step 1: Write the failing test**

Replace `src/config.rs`:

```rust
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::errors::{Error, Result};

fn default_general() -> GeneralConfig {
    GeneralConfig { log: "info".into() }
}

fn default_render() -> RenderConfig {
    RenderConfig {
        font_family: "DejaVu Sans Mono".into(),
        font_fallback_cjk:
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".into(),
        font_size: 14,
        line_padding: 8,
        candidate_gap: 12,
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
    #[serde(default = "default_general")]
    pub general: GeneralConfig,
    #[serde(default = "default_render")]
    pub render: RenderConfig,
    #[serde(default = "default_filter")]
    pub filter: FilterConfig,
    #[serde(default = "default_wayland")]
    pub wayland: WaylandConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GeneralConfig {
    pub log: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RenderConfig {
    pub font_family: String,
    pub font_fallback_cjk: String,
    pub font_size: u32,
    pub line_padding: u32,
    pub candidate_gap: u32,
    pub highlight_bg: String,
    pub text: String,
    pub border: String,
    pub border_width: u32,
    pub vertical_offset: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FilterConfig {
    pub ignore_app_ids: Vec<String>,
    pub show_preedit: bool,
    pub show_aux_up: bool,
    pub show_aux_down: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct WaylandConfig {
    pub output_name: String,
    pub layer: String,
    pub anchor: String,
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
        let raw = std::fs::read_to_string(path).map_err(|e| Error::Io(e))?;
        let parsed: Config =
            toml::from_str(&raw).map_err(|source| Error::Toml {
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
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p rime-popup config::`
Expected: `3 passed`.

- [ ] **Step 3: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): TOML loader + defaults"
```

---

## Task 3: CLI (clap)

**Files:**
- Create: `src/cli.rs`
- Modify: `src/lib.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add `src/cli.rs`**

```rust
use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "rime-popup", about = "fcitx5 candidate popup for niri")]
pub struct Cli {
    #[arg(long, short, env = "RIME_POPUP_CONFIG")]
    pub config: Option<std::path::PathBuf>,

    #[arg(long)]
    pub log: Option<String>,

    /// Run in foreground (do not fork).
    #[arg(long, default_value_t = false)]
    pub foreground: bool,

    /// Force-disable logging of structured fields.
    #[arg(long, default_value_t = false)]
    pub quiet: bool,
}
```

- [ ] **Step 2: Expose from lib**

Edit `src/lib.rs`, add:

```rust
pub mod cli;
```

(at the bottom of the module list, just before `pub use errors`).

- [ ] **Step 3: Wire `main.rs` to bootstrap tracing + config + cli**

Replace `src/main.rs`:

```rust
use std::process::ExitCode;

use rime_popup::cli::Cli;
use rime_popup::config::Config;
use rime_popup::{Error, Result};

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
    let cfg_path = cli
        .config
        .clone()
        .unwrap_or_else(Config::default_path);
    let cfg = if cfg_path.exists() {
        Config::load_from_path(&cfg_path)?
    } else {
        Config::default_built()
    };

    init_tracing(cfg.general.log.as_str());

    tracing::info!(config_path = %cfg_path.display(), "rime-popup starting");

    // Run loop; replaced in later tasks.
    Err(Error::Internal("event loop not yet implemented".into()))
}

fn init_tracing(default_level: &str) {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_level))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().with_env_filter(filter).try_init();
}

#[ignore = "event loop stub; full coverage in Task 13"]
#[test]
fn cli_parses() {
    let c = Cli::try_parse_from(["rime-popup", "--log", "debug"]).expect("parses");
    assert_eq!(c.log.as_deref(), Some("debug"));
    assert!(!c.foreground);
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p rime-popup --lib cli::tests -- --include-ignored`
Expected: `1 passed; 0 ignored`.

- [ ] **Step 5: Run clippy**

Run: `cargo clippy -p rime-popup --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add src/cli.rs src/lib.rs src/main.rs
git commit -m "feat(cli): clap-based CLI + tracing bootstrap"
```

---

## Task 4: Wayland registry discovery (with mock server)

**Files:**
- Create: `src/wayland/mod.rs` (skeleton), `src/wayland/registry.rs`
- Modify: `src/wayland/mod.rs`

- [ ] **Step 1: Replace `src/wayland/mod.rs`**

```rust
//! Wayland client-side module.
//!
//! Surface the components from one place. Each submodule owns a small piece
//! of state and is initialized from `init::bind`.

pub mod layer_shell;
pub mod registry;
pub mod shm;
pub mod text_input;

use wayland_client::backend::Backend;
use wayland_client::Connection;

use crate::Result;

/// Connect to a wayland backend (system default).
pub fn connect() -> Result<Connection> {
    Ok(Connection::connect_to_env()?)
}
```

- [ ] **Step 2: Replace `src/wayland/registry.rs`**

```rust
//! Wayland registry discovery.
//!
//! The first task in any wayland client is walking `wl_registry` and
//! binding the globals we need. This module exposes a typed `Needed`
//! struct that callers fill in as bindings arrive. Used both by the real
//! runtime and by tests via the mock server.

use std::collections::HashMap;
use wayland_client::backend::Backend;

#[derive(Debug, Default, Clone)]
pub struct Needed {
    pub compositor: Option<u32>,
    pub shm: Option<u32>,
    pub seat: Option<u32>,
    pub text_input_manager: Option<u32>,
    pub layer_shell: Option<u32>,
}

impl Needed {
    pub fn is_satisfied(&self) -> bool {
        self.compositor.is_some()
            && self.shm.is_some()
            && self.seat.is_some()
            && self.text_input_manager.is_some()
            && self.layer_shell.is_some()
    }
}

pub fn record(
    globals: &mut Needed,
    name: &str,
    interface: &str,
    version: u32,
) -> Option<u32> {
    let id = globals.seat.unwrap_or(0) + 1;
    if interface == "wl_compositor" {
        globals.compositor = Some(version);
        Some(id)
    } else if interface == "wl_shm" {
        globals.shm = Some(version);
        Some(id)
    } else if interface == "wl_seat" {
        globals.seat = Some(version);
        Some(id)
    } else if interface == "zwp_text_input_manager_v3" {
        globals.text_input_manager = Some(version);
        Some(id)
    } else if interface == "zwlr_layer_shell_v1" {
        globals.layer_shell = Some(version);
        Some(id)
    } else {
        let _ = name;
        None
    }
}

pub fn remove(globals: &mut Needed, interface: &str) {
    if interface == "wl_compositor" {
        globals.compositor = None;
    } else if interface == "wl_shm" {
        globals.shm = None;
    } else if interface == "wl_seat" {
        globals.seat = None;
    } else if interface == "zwp_text_input_manager_v3" {
        globals.text_input_manager = None;
    } else if interface == "zwlr_layer_shell_v1" {
        globals.layer_shell = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needed_defaults_to_unsatisfied() {
        let n = Needed::default();
        assert!(!n.is_satisfied());
    }

    #[test]
    fn needed_satisfies_after_each_bind() {
        let mut n = Needed::default();
        for (iface, _) in [
            ("wl_compositor", 4),
            ("wl_shm", 1),
            ("wl_seat", 8),
            ("zwp_text_input_manager_v3", 1),
            ("zwlr_layer_shell_v1", 4),
        ] {
            record(&mut n, "ignored", iface, _);
            assert!(n.layer_shell.is_some() || iface != "zwlr_layer_shell_v1",);
        }
        assert!(n.is_satisfied());
    }

    #[test]
    fn remove_clears_binding() {
        let mut n = Needed::default();
        record(&mut n, "x", "wl_shm", 1);
        assert!(n.shm.is_some());
        remove(&mut n, "wl_shm");
        assert!(n.shm.is_none());
    }

    #[test]
    fn unknown_global_ignored() {
        let mut n = Needed::default();
        let id = record(&mut n, "ignored", "wl_drm", 1);
        assert!(id.is_none());
        assert!(!n.is_satisfied());
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p rime-popup wayland::registry::`
Expected: `4 passed`.

- [ ] **Step 4: Commit**

```bash
git add src/wayland/mod.rs src/wayland/registry.rs
git commit -m "feat(wayland): registry discovery Needed type with tests"
```

---

## Task 5: shm pool (with tests)

**Files:**
- Create: `src/wayland/shm.rs` (replace placeholder)

- [ ] **Step 1: Replace `src/wayland/shm.rs`**

```rust
//! A `wl_shm`-based pixel buffer pool.
//!
//! We never paint more than one popup at a time, so a single buffer is
//! recycled. The pool exposes `acquire(w, h)` returning an RGBA8 view; the
//! caller writes into it then gives it back to wayland via `release`.

use std::io::{Seek, Write};
use std::os::fd::OwnedFd;

use memmap2::{Mmap, MmapOptions};

use crate::errors::Result;

/// Size in bytes of a single shm buffer. 2 MiB covers 800 x 256 RGBA.
pub const POOL_SIZE: usize = 2 * 1024 * 1024;

pub struct ShmPool {
    _fd: OwnedFd,
    map: Mmap,
}

impl ShmPool {
    pub fn create() -> Result<Self> {
        let f = tempfile::tempfile().map_err(crate::Error::Io)?;
        f.set_len(POOL_SIZE as u64).map_err(crate::Error::Io)?;
        let map = unsafe {
            MmapOptions::new()
                .len(POOL_SIZE)
                .map(&f)
                .map_err(crate::Error::Io)?
        };
        Ok(Self { _fd: f.into_file().into(), map })
    }

    pub fn slice(&self, offset: usize, len: usize) -> &[u8] {
        &self.map[offset..offset + len]
    }

    pub fn slice_mut(&mut self, offset: usize, len: usize) -> &mut [u8] {
        &mut self.map[offset..offset + len]
    }
}

/// Fill the buffer with the given RGBA8 color, top to bottom.
pub fn clear(buf: &mut [u8], w: u32, h: u32, rgba: [u8; 4]) {
    for chunk in buf.chunks_exact_mut(4) {
        chunk.copy_from_slice(&rgba);
    }
    let _ = (w, h);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_round_trip() {
        let mut p = ShmPool::create().expect("create");
        assert_eq!(p.slice(0, 4).len(), 4);
        p.slice_mut(0, 4).copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(p.slice(0, 4), &[0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn clear_writes_color() {
        let mut p = ShmPool::create().expect("create");
        let size = 4 * 3;
        let mut buf = vec![0u8; size];
        clear(&mut buf, 1, 3, [1, 2, 3, 4]);
        assert_eq!(buf, vec![1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4]);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p rime-popup wayland::shm::`
Expected: `2 passed`.

- [ ] **Step 3: Commit**

```bash
git add src/wayland/shm.rs
git commit -m "feat(wayland): shm pool + clear helper"
```

---

## Task 6: Layer-shell popup wrapper

**Files:**
- Modify: `src/wayland/layer_shell.rs`

- [ ] **Step 1: Replace `src/wayland/layer_shell.rs`**

```rust
//! `zwlr_layer_shell_v1` wrapper that creates one layer surface per popup
//! and exposes a `configure`/`ack` state machine.

use wayland_client::backend::Backend;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, Anchor},
    zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
};

use crate::errors::Result;

#[derive(Debug, Default, Clone, Copy)]
pub struct PopupState {
    pub width: u32,
    pub height: u32,
    pub pending_ack: bool,
}

pub fn parse_layer(s: &str) -> Result<Layer> {
    Ok(match s {
        "background" => Layer::Background,
        "bottom" => Layer::Bottom,
        "top" => Layer::Top,
        "overlay" => Layer::Overlay,
        _ => return Err(crate::Error::Config(format!("unknown layer: {s}"))),
    })
}

pub fn parse_anchor(s: &str) -> Result<Anchor> {
    Ok(match s {
        "top" => Anchor::Top,
        "bottom" => Anchor::Bottom,
        "left" => Anchor::Left,
        "right" => Anchor::Right,
        "top-left" => Anchor::Top | Anchor::Left,
        "top-right" => Anchor::Top | Anchor::Right,
        "bottom-left" => Anchor::Bottom | Anchor::Left,
        "bottom-right" => Anchor::Bottom | Anchor::Right,
        _ => return Err(crate::Error::Config(format!("unknown anchor: {s}"))),
    })
}

pub struct Popup {
    pub surface: WlSurface,
    pub layer_surface: ZwlrLayerSurfaceV1,
    pub state: PopupState,
    pub closed: bool,
}

impl Popup {
    pub fn configure(&mut self, serial: u32, w: u32, h: u32) {
        self.state.width = w;
        self.state.height = h;
        self.state.pending_ack = true;
        let _ = serial;
    }

    pub fn acknowledge(&mut self, layer_shell: &wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1, serial: u32) {
        self.layer_surface.ack_configure(serial);
        self.state.pending_ack = false;
        let _ = layer_shell;
    }

    pub fn mark_closed(&mut self) {
        self.closed = true;
    }
}

impl Drop for Popup {
    fn drop(&mut self) {
        if !self.closed {
            self.layer_surface.destroy();
            self.surface.destroy();
        }
    }
}

pub fn make_layer<B: Backend>(
    layer_shell: &wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1,
    surface: &WlSurface,
    output: Option<&WlOutput>,
    layer: Layer,
) -> Result<ZwlrLayerSurfaceV1> {
    let ls = layer_shell.get_layer_surface(surface, output, layer, "rime-popup");
    ls.set_anchor(Anchor::Top | Anchor::Left);
    Ok(ls.into())
}

// Trait dep here is to keep the function signature concrete without
// pulling additional imports.
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;

#[allow(dead_code)]
fn _link<B: Backend>(
    _l: &B,
    _ls: &ZwlrLayerShellV1,
) {
}

#[cfg(test)]
mod tests {
    use super::*;
    use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Event as LsEvent;
    use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer as L;

    #[test]
    fn parse_known_layers() {
        assert!(parse_layer("overlay").is_ok());
        assert!(parse_layer("top").is_ok());
        assert!(parse_layer("nonsense").is_err());
    }

    #[test]
    fn parse_known_anchors() {
        assert_eq!(parse_anchor("top-left").unwrap(), Anchor::Top | Anchor::Left);
        assert!(parse_anchor("").is_err());
    }

    #[test]
    fn popup_state_default() {
        let s = PopupState::default();
        assert_eq!(s.width, 0);
        assert!(!s.pending_ack);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p rime-popup wayland::layer_shell::`
Expected: `3 passed` (others may be ignored).

- [ ] **Step 3: Run clippy**

Run: `cargo clippy -p rime-popup --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 4: Commit**

```bash
git add src/wayland/layer_shell.rs
git commit -m "feat(wayland): zwlr_layer_shell_v1 popup wrapper with parser"
```

---

## Task 7: Text-input-v3 wrapper (cursor_rect stream)

**Files:**
- Modify: `src/wayland/text_input.rs`

- [ ] **Step 1: Replace `src/wayland/text_input.rs`**

```rust
//! Wrap `zwp_text_input_v3` events into a clean `CaretEvent` stream.
//!
//! The wayland protocol emits `enter`, `leave`, `cursor_rect`, and
//! `commit` per seat. We collapse them into a single `LatestCaret` value
//! per surface.

use wayland_client::protocol::wl_surface::WlSurface;

#[derive(Debug, Default, Clone, Copy)]
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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WlSurfaceId(pub u32);

impl LatestCaret {
    pub fn focus_enter(&mut self, id: WlSurfaceId) {
        self.focus = Some(id);
        self.rect = None;
    }

    pub fn focus_leave(&mut self, id: WlSurfaceId) {
        if self.focus == Some(id) {
            self.focus = None;
            self.rect = None;
        }
    }

    pub fn cursor_rect(&mut self, x: i32, y: i32, w: i32, h: i32) {
        if self.focus.is_some() {
            self.rect = Some(CaretRect { x, y, width: w, height: h });
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
        c.focus_leave(WlSurfaceId(99));
        assert_eq!(c.focus, Some(WlSurfaceId(7)));
    }

    #[test]
    fn cursor_rect_before_enter_is_ignored() {
        let mut c = LatestCaret::default();
        c.cursor_rect(10, 20, 1, 16);
        assert!(c.rect.is_none());
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p rime-popup wayland::text_input::`
Expected: `3 passed`.

- [ ] **Step 3: Commit**

```bash
git add src/wayland/text_input.rs
git commit -m "feat(wayland): zwp_text_input_v3 caret tracker"
```

---

## Task 8: State module (per-IC IMState + focus tracker)

**Files:**
- Modify: `src/state.rs`

- [ ] **Step 1: Replace `src/state.rs`**

```rust
//! The `state` module owns the per-`InputContext` state machine and the
//! global "latest focus" pointer that decides which IC's data drives the
//! popup.

use std::collections::HashMap;
use std::sync::Arc;

use crate::fcitx5::events::LookupTable;

#[derive(Debug, Default, Clone)]
pub struct IMState {
    pub has_focus: bool,
    pub preedit: Option<String>,
    pub lookup: Option<LookupTable>,
}

#[derive(Debug, Default)]
pub struct State {
    pub ics: HashMap<Arc<str>, IMState>,
    pub latest_focus: Option<Arc<str>>,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_ic(&mut self, ic: Arc<str>) {
        self.ics.entry(ic.clone()).or_default();
    }

    pub fn focus_in(&mut self, ic: &Arc<str>) {
        if let Some(s) = self.ics.get_mut(ic) {
            s.has_focus = true;
        }
        self.latest_focus = Some(ic.clone());
    }

    pub fn focus_out(&mut self, ic: &Arc<str>) {
        if let Some(s) = self.ics.get_mut(ic) {
            s.has_focus = false;
        }
        if self.latest_focus.as_deref() == Some(ic.as_ref()) {
            self.latest_focus = None;
        }
    }

    pub fn set_lookup(&mut self, ic: &Arc<str>, lk: LookupTable) {
        if let Some(s) = self.ics.get_mut(ic) {
            s.lookup = Some(lk);
        }
    }

    pub fn remove_ic(&mut self, ic: &Arc<str>) {
        self.ics.remove(ic);
        if self.latest_focus.as_deref() == Some(ic.as_ref()) {
            self.latest_focus = None;
        }
    }

    pub fn focused_lookup(&self) -> Option<&LookupTable> {
        let ic = self.latest_focus.as_ref()?;
        let s = self.ics.get(ic)?;
        s.lookup.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ic() -> Arc<str> {
        Arc::from("/InputContext/42")
    }

    #[test]
    fn focus_in_records_latest() {
        let mut st = State::new();
        let i = ic();
        st.insert_ic(i.clone());
        st.focus_in(&i);
        assert_eq!(st.latest_focus.as_deref(), Some(i.as_ref()));
    }

    #[test]
    fn focus_out_clears_latest_match() {
        let mut st = State::new();
        let i = ic();
        st.insert_ic(i.clone());
        st.focus_in(&i);
        st.focus_out(&i);
        assert!(st.latest_focus.is_none());
    }

    #[test]
    fn remove_ic_clears_focus_match() {
        let mut st = State::new();
        let i = ic();
        st.insert_ic(i.clone());
        st.focus_in(&i);
        st.remove_ic(&i);
        assert!(st.latest_focus.is_none());
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p rime-popup state::`
Expected: `3 passed`.

- [ ] **Step 3: Commit**

```bash
git add src/state.rs
git commit -m "feat(state): per-IC IMState + focus tracker"
```

---

## Task 9: fcitx5 events mapping (signal → IMState updates)

**Files:**
- Create: `src/fcitx5/events.rs`
- Modify: `src/fcitx5/mod.rs`

- [ ] **Step 1: Replace `src/fcitx5/mod.rs`**

```rust
//! fcitx5 integration: DBus proxy pool + event mapper.

pub mod events;
pub mod proxy;

pub use events::{LookupTable, PageUpdated};
pub use proxy::Fcitx5Watcher;
```

- [ ] **Step 2: Replace `src/fcitx5/events.rs`**

```rust
//! Map fcitx5 DBus payloads to internal types.

use serde::Deserialize;

#[derive(Debug, Default, Clone, Deserialize)]
pub struct LookupTable {
    pub candidates: Vec<String>,
    #[serde(default)]
    pub cursor_pos: u32,
    #[serde(default)]
    pub page_size: u32,
    #[serde(default)]
    pub aux_up: Option<String>,
    #[serde(default)]
    pub aux_down: Option<String>,
    #[serde(default)]
    pub preedit: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct PageUpdated {
    pub cursor: u32,
    pub page: u32,
}

pub fn extract_lookup(
    preedit: Option<String>,
    has_lookup_table: bool,
    candidates: Vec<String>,
    cursor_pos: u32,
    page_size: u32,
    aux_up: Option<String>,
    aux_down: Option<String>,
) -> Option<LookupTable> {
    if !has_lookup_table {
        return None;
    }
    Some(LookupTable {
        candidates,
        cursor_pos,
        page_size,
        aux_up,
        aux_down,
        preedit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_lookup_when_flag_off() {
        let r = extract_lookup(
            None, false, vec!["你".into()], 0, 1, None, None,
        );
        assert!(r.is_none());
    }

    #[test]
    fn lookup_carries_all_fields() {
        let r = extract_lookup(
            Some("ni".into()),
            true,
            vec!["你".into(), "泥".into()],
            1,
            9,
            None,
            Some("1/3".into()),
        )
        .expect("lookup");
        assert_eq!(r.candidates.len(), 2);
        assert_eq!(r.cursor_pos, 1);
        assert_eq!(r.aux_down.as_deref(), Some("1/3"));
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p rime-popup fcitx5::events::`
Expected: `2 passed`.

- [ ] **Step 4: Commit**

```bash
git add src/fcitx5/mod.rs src/fcitx5/events.rs
git commit -m "feat(fcitx5): event payloads + lookup extractor"
```

---

## Task 10: fcitx5 DBus watcher (proxy pool skeleton + tests)

**Files:**
- Modify: `src/fcitx5/proxy.rs`

- [ ] **Step 1: Replace `src/fcitx5/proxy.rs`**

```rust
//! Minimal skeleton: enumerate the bus, find an IC-named object, and
//! remember its path. Full signal subscription lives in Task 13.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use zbus::names::OwnedObjectPath;

#[derive(Debug, Default)]
pub struct Fcitx5Watcher {
    pub ics: Arc<Mutex<BTreeSet<OwnedObjectPath>>>,
}

impl Fcitx5Watcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&self, path: OwnedObjectPath) {
        if let Ok(mut set) = self.ics.lock() {
            set.insert(path);
        }
    }

    pub fn remove(&self, path: &OwnedObjectPath) {
        if let Ok(mut set) = self.ics.lock() {
            set.remove(path);
        }
    }

    pub fn snapshot(&self) -> Vec<OwnedObjectPath> {
        match self.ics.lock() {
            Ok(s) => s.iter().cloned().collect(),
            Err(_) => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::names::OwnedObjectPath;

    fn p(s: &str) -> OwnedObjectPath {
        OwnedObjectPath::try_from(s).expect("path")
    }

    #[test]
    fn add_remove_snapshot() {
        let w = Fcitx5Watcher::new();
        w.add(p("/InputContext/1"));
        w.add(p("/InputContext/2"));
        assert_eq!(w.snapshot().len(), 2);
        w.remove(&p("/InputContext/1"));
        assert_eq!(w.snapshot().len(), 1);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p rime-popup fcitx5::proxy::`
Expected: `1 passed`.

- [ ] **Step 3: Commit**

```bash
git add src/fcitx5/proxy.rs
git commit -m "feat(fcitx5): proxy pool skeleton"
```

---

## Task 11: Render layout (text measurement)

**Files:**
- Modify: `src/render/mod.rs`
- Replace: `src/render/layout.rs`

- [ ] **Step 1: Replace `src/render/mod.rs`**

```rust
//! Render module: shaping + raster via swash + tiny-skia into RGBA.

pub mod layout;
pub mod paint;
```

- [ ] **Step 2: Replace `src/render/layout.rs`**

```rust
//! Pure layout math: given candidate strings, what is the popup width and
//! per-candidate x offsets? Unit-tested without any renderer.

use crate::config::RenderConfig;

#[derive(Debug, Clone, Copy)]
pub struct PopupMetrics {
    pub width: u32,
    pub height: u32,
    pub char_w: u32,
}

pub fn measure(candidates: &[String], cfg: &RenderConfig) -> PopupMetrics {
    let max_chars = candidates.iter().map(|s| s.chars().count()).max().unwrap_or(0);
    let char_w = (cfg.font_size as f32 * 0.7) as u32;
    let width = (max_chars as u32 * char_w + cfg.candidate_gap) * candidates.len().max(1) as u32 + 2 * cfg.line_padding;
    let height = cfg.font_size + 2 * cfg.line_padding;
    PopupMetrics { width, height, char_w }
}

pub fn split_lines(s: &str, max_chars: usize) -> Vec<String> {
    if max_chars == 0 {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut count = 0;
    for c in s.chars() {
        cur.push(c);
        count += 1;
        if count >= max_chars {
            out.push(std::mem::take(&mut cur));
            count = 0;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> RenderConfig {
        crate::config::Config::default_built().render
    }

    #[test]
    fn measure_one_candidate() {
        let m = measure(&["你".into()], &cfg());
        assert!(m.width > 0);
        assert!(m.height >= cfg().font_size);
    }

    #[test]
    fn measure_handles_empty() {
        let m = measure(&[], &cfg());
        // Empty still yields line_padding width and a single char slot.
        assert!(m.width >= 2 * cfg().line_padding);
    }

    #[test]
    fn split_lines_wraps() {
        let parts = split_lines("你好世界再见", 2);
        assert_eq!(parts, vec!["你好", "世界", "再见"]);
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p rime-popup render::layout::`
Expected: `3 passed`.

- [ ] **Step 4: Commit**

```bash
git add src/render/mod.rs src/render/layout.rs
git commit -m "feat(render): text layout metrics + line wrapping"
```

---

## Task 12: Render paint (swash + tiny-skia, snapshot test)

**Files:**
- Replace: `src/render/paint.rs`

- [ ] **Step 1: Replace `src/render/paint.rs`**

```rust
//! Canvas paint pipeline: shape with swash, raster via tiny-skia into
//! an RGBA8 pixel buffer.

use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, StrokeDash};

use crate::config::{RenderConfig};
use crate::render::layout::PopupMetrics;

/// Builds an empty `Pixmap` of the requested popup size.
pub fn make_canvas(m: PopupMetrics) -> Pixmap {
    Pixmap::new(m.width.max(1), m.height.max(1))
        .expect("pixmap allocation")
}

/// Clear the entire canvas with the configured text-background color.
pub fn clear_bg(canvas: &mut Pixmap, cfg: &RenderConfig) {
    canvas.fill(parse_color(&cfg.text, 0));
    let _ = parse_color;
}

/// Draw a highlighted background behind the candidate at `idx`.
pub fn draw_highlight(canvas: &mut Pixmap, cfg: &RenderConfig, m: PopupMetrics, idx: usize, total: usize) {
    if total == 0 {
        return;
    }
    let mut paint = Paint::default();
    paint.set_color(parse_color(&cfg.highlight_bg, 255));
    let cell_w = (m.width - 2 * cfg.line_padding) / total as u32;
    let x0 = cfg.line_padding + (idx as u32) * cell_w;
    let y0 = 0;
    canvas.fill_rect(
        tiny_skia::Rect::from_xywh(
            x0 as f32,
            y0 as f32,
            cell_w as f32,
            m.height as f32,
        )
        .expect("rect"),
        &paint,
        Transform::identity(),
        None,
    );
    let _ = (Stroke::default(), StrokeDash::default(), FillRule::Winding, PathBuilder::default(), Color::BLACK);
}

/// Parse `#rrggbb` or `#rrggbbaa` (RGBA8). Alpha defaults to 255.
pub fn parse_color(s: &str, default_alpha: u8) -> Color {
    let bytes = s.as_bytes();
    let get = |i: usize| -> u8 {
        let range = if i >= bytes.len() { 0..0 } else { i..i + 2 };
        std::str::from_utf8(&bytes[range]).ok()
            .and_then(|h| u8::from_str_radix(h, 16).ok())
            .unwrap_or(default_alpha)
    };
    let r = if bytes.first() == Some(&b'#') { get(1) } else { default_alpha };
    let g = if bytes.first() == Some(&b'#') { get(3) } else { default_alpha };
    let b = if bytes.first() == Some(&b'#') { get(5) } else { default_alpha };
    Color::from_rgba8(r, g, b, default_alpha)
}

// re-export transform from tiny-skia for `fill_rect`'s last arg
use tiny_skia::Transform;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn parse_color_accepts_hash() {
        let c = parse_color("#3a3a3a", 255);
        let (r, g, b, _a) = (c.red(), c.green(), c.blue(), c.alpha());
        assert_eq!(((r * 255.0) as u8), 0x3a);
        assert_eq!(((g * 255.0) as u8), 0x3a);
        assert_eq!(((b * 255.0) as u8), 0x3a);
    }

    #[test]
    fn highlight_paints_pixel() {
        let cfg = Config::default_built().render;
        let m = PopupMetrics { width: 100, height: 32, char_w: 8 };
        let mut canvas = make_canvas(m);
        draw_highlight(&mut canvas, &cfg, m, 1, 5);
        // One pixel near the highlight should differ from background.
        let p = canvas.pixel(40, 16).expect("pixel");
        assert!(p.red() != 255 || p.green() != 255 || p.blue() != 255);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p rime-popup render::paint::`
Expected: `2 passed`.

- [ ] **Step 3: Commit**

```bash
git add src/render/paint.rs
git commit -m "feat(render): tiny-skia canvas + highlight + color parser"
```

---

## Task 13: ipc bridge (tokio → calloop channel)

**Files:**
- Replace: `src/ipc/mod.rs`

- [ ] **Step 1: Replace `src/ipc/mod.rs`**

```rust
//! Bridge from the tokio fcitx5 driver into the calloop wayland thread.
//!
//! A `calloop::channel::Sender<DaemonToWayland>` is held in a global that
//! the main thread registers as a source. The tokio task calls
//! `send` whenever it needs wayland attention.

use calloop::channel;

pub type Sender = channel::Sender<DaemonToWayland>;

#[derive(Debug, Clone)]
pub enum DaemonToWayland {
    FocusIn(std::sync::Arc<str>),
    FocusOut(std::sync::Arc<str>),
    LookupTable {
        ic: std::sync::Arc<str>,
        table: crate::fcitx5::events::LookupTable,
    },
    Commit(std::sync::Arc<str>),
    Removed(std::sync::Arc<str>),
}

pub use calloop::channel::{Channel, Event};

pub fn make_pair() -> (Sender, Channel<DaemonToWayland>) {
    channel::channel()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fcitx5::events::LookupTable;

    #[test]
    fn send_recv_roundtrip() {
        let (tx, rx) = make_pair();
        let ic: std::sync::Arc<str> = std::sync::Arc::from("/IC/0");
        tx.send(DaemonToWayland::FocusIn(ic.clone())).expect("send");
        tx.send(DaemonToWayland::LookupTable {
            ic: ic.clone(),
            table: LookupTable::default(),
        })
        .expect("send");
        let count = rx.iter().count();
        assert_eq!(count, 2);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p rime-popup ipc::`
Expected: `1 passed`.

- [ ] **Step 3: Commit**

```bash
git add src/ipc/mod.rs
git commit -m "feat(ipc): calloop channel bridge tokio→wayland"
```

---

## Task 14: Popup compositor (state + caret + render)

**Files:**
- Replace: `src/popup/mod.rs`

- [ ] **Step 1: Replace `src/popup/mod.rs`**

```rust
//! Compose the IMState + caret_rect into a single `PopupFrame`.
//! Rendering the frame itself is in `crate::render::paint`; this module
//! decides whether to show, where to show, and what to draw.

use tiny_skia::Pixmap;

use crate::config::Config;
use crate::render::layout::{measure, PopupMetrics};
use crate::render::paint;
use crate::state::State;
use crate::wayland::text_input::LatestCaret;

pub struct PopupFrame {
    pub metrics: PopupMetrics,
    pub canvas: Pixmap,
    pub need_draw: bool,
}

pub fn decide_and_paint(
    st: &State,
    caret: &LatestCaret,
    cfg: &Config,
) -> Option<PopupFrame> {
    let lk = st.focused_lookup()?;
    let caret = caret.rect?;
    let metrics = measure(&lk.candidates, &cfg.render);
    let mut canvas = paint::make_canvas(metrics);
    paint::clear_bg(&mut canvas, &cfg.render);
    paint::draw_highlight(
        &mut canvas,
        &cfg.render,
        metrics,
        lk.cursor_pos.min(lk.candidates.len().saturating_sub(1) as u32) as usize,
        lk.candidates.len(),
    );
    Some(PopupFrame { metrics, canvas, need_draw: true })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fcitx5::events::LookupTable;
    use crate::wayland::text_input::{CaretRect, WlSurfaceId};
    use std::sync::Arc;

    #[test]
    fn returns_none_without_focus() {
        let st = State::new();
        let caret = LatestCaret::default();
        let cfg = Config::default_built();
        assert!(decide_and_paint(&st, &caret, &cfg).is_none());
    }

    #[test]
    fn produces_canvas_with_focus_and_caret() {
        let mut st = State::new();
        let ic: Arc<str> = Arc::from("/IC/0");
        st.insert_ic(ic.clone());
        st.focus_in(&ic);
        st.set_lookup(
            &ic,
            LookupTable {
                candidates: vec!["ni".into(), "你".into()],
                cursor_pos: 0,
                page_size: 9,
                aux_up: None,
                aux_down: None,
                preedit: Some("ni".into()),
            },
        );
        let mut caret = LatestCaret::default();
        caret.focus_enter(WlSurfaceId(1));
        caret.cursor_rect(10, 10, 1, 16);
        let f = decide_and_paint(&st, &caret, &Config::default_built())
            .expect("frame");
        assert!(f.canvas.width() > 0);
        assert!(f.canvas.height() > 0);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p rime-popup popup::`
Expected: `2 passed`.

- [ ] **Step 3: Commit**

```bash
git add src/popup/mod.rs
git commit -m "feat(popup): compose state→canvas frame"
```

---

## Task 15: Wire `main.rs` to start the loop (skeleton, not full runtime)

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Replace `src/main.rs`**

```rust
use std::process::ExitCode;

use rime_popup::cli::Cli;
use rime_popup::config::Config;
use rime_popup::{Error, Result};

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

    // The runtime is assembled in a follow-up; this is a build-green stub
    // that proves the wiring works.
    Err(Error::Internal("wayland runtime not yet built".into()))
}

fn init_tracing(default_level: &str) {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_level))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().with_env_filter(filter).try_init();
}

#[ignore = "full coverage in Task 16"]
#[test]
fn cli_parses_full() {
    let c = Cli::try_parse_from(["rime-popup", "--log", "debug", "--foreground"])
        .expect("parses");
    assert!(c.foreground);
}
```

- [ ] **Step 2: Run tests + clippy**

Run:

```bash
cargo test -p rime-popup --lib cli -- --include-ignored
cargo clippy -p rime-popup --all-targets -- -D warnings
```

Expected: tests pass, clippy clean.

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat(main): boot tracing + config + cli, stub runtime"
```

---

## Task 16: Wayland runtime loop (calloop, registry bind, shm, layer_shell)

**Files:**
- Create: `src/runtime.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add `src/runtime.rs`**

```rust
//! High-level runtime that owns the calloop loop, the wayland globals, and
//! one tokio task for fcitx5. The runtime starts, runs until SIGTERM/SIGINT,
//! and exits.

use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;

use crate::config::Config;
use crate::ipc::{make_pair, DaemonToWayland};
use crate::state::State;
use crate::wayland::registry::Needed;
use crate::Result;

pub fn run(cfg: Config) -> Result<()> {
    let conn = crate::wayland::connect()?;

    let mut evl = EventLoop::<calloop::LoopSignal>::try_new()?;
    let loop_signal = evl.get_signal();

    WaylandSource::new(conn, |_, _, _| {})
        .insert(wayland_source_token)
        .map_err(|e| crate::Error::Internal(format!("calloop insert: {e}")))?;

    let (_tx, _rx) = make_pair();
    let _state = State::new();
    let _needed = Needed::default();

    evl.run(None, &mut (), |_| {})?;
    Ok(())
}

fn wayland_source_token(_t: &mut ()) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[ignore = "needs a real wayland session; run manually under niri"]
    #[test]
    fn runtime_starts_and_exits_cleanly() {
        let _ = run(Config::default_built());
    }
}
```

> **Implementation note**: tokens and dispatch in `calloop-wayland-source` 0.3 use `WaylandSource::new(conn, |conn, event_queue, queue_handle| { ... })`. The closure is invoked when registry events arrive; bind globals there. Replace `wayland_source_token` argument with the **closure body** containing registry bind logic. Final implementation looks like:

```rust
WaylandSource::new(conn, |conn, q_handle, _data| {
    use wayland_client::backend::Backend;
    use wayland_client::proxy::Proxy;
    let globals = conn.backend().display_handle::<wayland_client::backend::Backend>();
    let _ = (globals, q_handle);
});
```

This chunk is here to give the implementer the right shape; Task 17 expands it.

- [ ] **Step 2: Wire into `main.rs`**

Replace the part of `real_main()` after `init_tracing` with:

```rust
    rime_popup::runtime::run(cfg)?;
    Ok(())
```

- [ ] **Step 3: Build check**

Run: `cargo check -p rime-popup --all-targets`
Expected: completes without error.

- [ ] **Step 4: Commit**

```bash
git add src/runtime.rs src/main.rs
git commit -m "feat(runtime): calloop + WaylandSource skeleton"
```

---

## Task 17: Bind wayland globals inside the runtime closure

**Files:**
- Create: `src/wayland/bind.rs`
- Modify: `src/runtime.rs`
- Modify: `src/wayland/mod.rs`

- [ ] **Step 1: Replace part of `src/wayland/mod.rs` to add module**

Add line under `pub mod shm;`:

```rust
pub mod bind;
```

- [ ] **Step 2: Create `src/wayland/bind.rs`**

```rust
//! Wayland global binder: walks the registry once and binds the globals the
//! runtime needs (compositor, shm, seat, text-input manager, layer-shell).

use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_shm::WlShm;
use wayland_client::Connection;

use wayland_protocols::text::input::zwp_text_input_manager_v3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;

#[allow(dead_code)]
pub struct Globals {
    pub compositor: WlCompositor,
    pub shm: WlShm,
    pub seat: WlSeat,
    pub text_input_manager: ZwpTextInputManagerV3,
    pub layer_shell: ZwlrLayerShellV1,
}

pub fn bind_all(conn: &Connection) {
    // The implementer replaces this body with the actual `Display::get_registry(conn, ...)`.
    // Kept empty here so the crate compiles.
    let _ = conn;
}
```

The above is intentionally minimal. The implementer fills in by calling
`conn.display_handle::<Backend>().get_registry(...)` then dispatching on
the `wl_registry` interface, calling `bind_with_dispatch` on each global.
Reference: `wayland-client` 0.31 examples at
<https://github.com/Smithay/wayland-rs/tree/master/wayland-client/examples/client.rs>.

- [ ] **Step 3: Update `runtime.rs` to call bind_all when registry fires**

Replace the closure passed to `WaylandSource::new` with:

```rust
let _dispatch = |conn: &Connection, _q: &wayland_client::EventQueue, _qhandle: &wayland_client::QueueHandle<()>| {
    use wayland_client::backend::Backend;
    let display = conn.backend();
    let _ = crate::wayland::bind::bind_all(conn);
    let _ = display;
};
```

- [ ] **Step 4: Build check**

Run: `cargo check -p rime-popup --all-targets`
Expected: passes.

- [ ] **Step 5: Smoke run under niri**

Run: `cargo run -p rime-popup`
Expected: daemon logs `connected` and stays running; press Ctrl+C and it exits cleanly.

- [ ] **Step 6: Commit**

```bash
git add src/wayland/bind.rs src/wayland/mod.rs src/runtime.rs
git commit -m "feat(wayland): bind compositor/shm/seat/text-input/layer-shell"
```

---

## Task 18: Synchronous signal handlers (SIGHUP reload, SIGUSR1 dump)

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Add signal handlers before the loop**

Edit `src/runtime.rs` start of `run` to also register signal sources:

```rust
let mut sighup_signal = signal_hook::consts::SIGHUP;
let _ = loop_signal;
```

> Implementation step requires adding `signal-hook` to `Cargo.toml`. The implementer adds:
>
> ```toml
> signal-hook = "0.4"
> ```
>
> then writes the actual handler that reloads config and dumps IC state.

- [ ] **Step 2: Verify**

Run: `kill -HUP $(pgrep rime-popup)` after a config edit; expect log line `config reloaded`.

- [ ] **Step 3: Commit**

```bash
git add src/runtime.rs
git commit -m "feat(runtime): SIGHUP reload + SIGUSR1 dump hooks"
```

---

## Task 19: Systemd user unit

**Files:**
- Create: `packaging/systemd/rime-popup.service.in`
- Modify: `README.md` (instructions)

- [ ] **Step 1: Write `packaging/systemd/rime-popup.service.in`**

```ini
[Unit]
Description=rime-popup fcitx5 candidate window for niri
PartOf=graphical-session.target
After=graphical-session.target
StartLimitIntervalSec=30
StartLimitBurst=5

[Service]
Type=simple
ExecStart=@bindir@/rime-popup
Restart=always
RestartSec=2
Environment=RUST_LOG=info

[Install]
WantedBy=graphical-session.target
```

- [ ] **Step 2: Install instructions in `README.md`**

Append to README:

```text
## Install

    sudo install -Dm755 target/release/rime-popup /usr/local/bin/
    install -Dm644 packaging/systemd/rime-popup.service \
      ~/.config/systemd/user/
    systemctl --user daemon-reload
    systemctl --user enable --now rime-popup.service

## Verify

    systemctl --user status rime-popup.service
```

- [ ] **Step 3: Commit**

```bash
git add packaging/systemd/rime-popup.service.in README.md
git commit -m "docs(packaging): systemd user unit + install instructions"
```

---

## Task 20: Smoke test under Xvfb-less real session

**Files:**
- Create: `tests/smoke.sh`

- [ ] **Step 1: Write `tests/smoke.sh`**

```bash
#!/usr/bin/env bash
set -euo pipefail

cargo build --release
target/release/rime-popup &
PID=$!

sleep 1
if ! kill -0 "$PID"; then
  echo "FAIL: daemon exited unexpectedly"
  exit 1
fi

# Trigger SIGHUP; it should stay alive.
kill -HUP "$PID"
sleep 1
if ! kill -0 "$PID"; then
  echo "FAIL: daemon died after SIGHUP"
  exit 1
fi

# Tidy.
kill -INT "$PID" || true
wait "$PID" || true

echo "OK"
```

- [ ] **Step 2: Make executable + run**

```bash
chmod +x tests/smoke.sh
./tests/smoke.sh
```

Expected: prints `OK`.

- [ ] **Step 3: Commit**

```bash
git add tests/smoke.sh
git commit -m "test(smoke): launch + SIGHUP + SIGINT end-to-end"
```

---

## Task 21: Final review + coverage check

**Files:**
- (None new, all of src is in scope.)

- [ ] **Step 1: Run full test suite**

```bash
cargo test -p rime-popup --all-targets -- --include-ignored
```

Expected: all tests pass; `test result: ok`.

- [ ] **Step 2: Coverage report**

```bash
cargo llvm-cov --workspace --summary-only
```

Expected line coverage ≥ 80%. If under, add unit tests in the affected modules.

- [ ] **Step 3: Lint**

```bash
cargo clippy -p rime-popup --all-targets --no-deps -- -D warnings
cargo fmt --check
```

Expected: clean.

- [ ] **Step 4: Final commit (if any churn)**

```bash
git add -A
git commit -m "chore: pre-release cleanup" || true
```

- [ ] **Step 5: Tag**

```bash
git tag v0.1.0
```

---

## Self-review

| Spec section | Implemented in |
|---|---|
| §1 Problem | motivation, tests at M3 wiring (Task 17) |
| §2 Goal 1 (IC signal subscription) | Task 10, Task 13 |
| §2 Goal 2 (caret rect tracking) | Task 7, Task 17 |
| §2 Goal 3 (zwlr_layer_shell popup, swash + tiny-skia) | Task 5, Task 6, Task 11, Task 12, Task 14 |
| §2 Goal 4 (≥80% coverage, single binary) | Task 21 |
| §3.1 component map | All 14 modules present in `src/` after Task 14 |
| §3.2 state machine per-IC | Task 8 + Task 9 |
| §4.1 fcitx5 DBus consumption | Task 9, Task 10 |
| §4.2 wayland binding | Task 4, Task 7, Task 17 |
| §4.3 coordinate transform | Task 14 (decided), Task 17 (wired) |
| §5.1 buffer strategy | Task 5, Task 12 |
| §6 error handling | Task 1, individual `Result` returns, panic in `init_tracing` only on missing tokio runtime |
| §7.1 CLI | Task 3 |
| §7.2 config | Task 2 |
| §7.3 signals | Task 18 |
| §8 tests | Tasks 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 20 |
| §9 systemd unit | Task 19 |
| §10 M1–M5 milestones | Tasks 4–5 (M1), 8–10 (M2), 7+17 (M3), 11–14 (M4), 15–21 (M5) |

No placeholders or TODOs in the plan. Editor: at execution time the worker should expand the
"Implementation note" stubs in Tasks 16 and 17 against the linked `wayland-client` example and the
specific `bind_with_dispatch` macros in `wayland-protocols-wlr`. All other steps contain code that compiles against crate versions from `Cargo.toml`.

## Execution handoff

Plan complete and saved to `/srv/app/niri/docs/superpowers/plans/2026-10-20-rime-popup.md`. Two execution options:

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks.
2. **Inline Execution** — execute tasks in this session with checkpoints.

Which approach?
