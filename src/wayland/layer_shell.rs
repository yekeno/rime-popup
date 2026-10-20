//! `zwlr_layer_shell_v1` wrapper that creates one layer surface per popup
//! and exposes a `configure`/`ack_configure` state machine.

use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Dispatch, QueueHandle};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{
    Layer, ZwlrLayerShellV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    Anchor, ZwlrLayerSurfaceV1,
};

use crate::errors::Error;
use crate::Result;

#[derive(Debug, Default, Clone, Copy)]
pub struct PopupState {
    pub width: u32,
    pub height: u32,
    /// A `configure` has arrived but not yet been acknowledged. Wayland
    /// requires the ack before any commit will take effect.
    pub pending_ack: bool,
}

/// Parse a layer name from config into a `zwlr_layer_shell_v1` layer.
pub fn parse_layer(s: &str) -> Result<Layer> {
    match s {
        "background" => Ok(Layer::Background),
        "bottom" => Ok(Layer::Bottom),
        "top" => Ok(Layer::Top),
        "overlay" => Ok(Layer::Overlay),
        other => Err(Error::Config(format!("unknown layer: {other}"))),
    }
}

/// Parse an anchor from config. Composite anchors are bitwise unions.
pub fn parse_anchor(s: &str) -> Result<Anchor> {
    match s {
        "top" => Ok(Anchor::Top),
        "bottom" => Ok(Anchor::Bottom),
        "left" => Ok(Anchor::Left),
        "right" => Ok(Anchor::Right),
        "top-left" => Ok(Anchor::Top | Anchor::Left),
        "top-right" => Ok(Anchor::Top | Anchor::Right),
        "bottom-left" => Ok(Anchor::Bottom | Anchor::Left),
        "bottom-right" => Ok(Anchor::Bottom | Anchor::Right),
        other => Err(Error::Config(format!("unknown anchor: {other}"))),
    }
}

/// Create the layer surface for a popup.
///
/// wayland-rs 0.31 threads a `QueueHandle<D>` and a `udata: U` payload through
/// every request that creates a proxy; the surface's events are delivered to
/// that queue, and `udata` is what a `Dispatch` impl gets back on each event.
/// Both are caller-supplied because the runtime owns the queue and decides
/// what state a surface should carry.
pub fn make_layer<D, U>(
    layer_shell: &ZwlrLayerShellV1,
    qh: &QueueHandle<D>,
    udata: U,
    spec: LayerSpec<'_>,
) -> ZwlrLayerSurfaceV1
where
    U: Send + Sync + 'static,
    D: Dispatch<ZwlrLayerSurfaceV1, U> + 'static,
{
    let ls = layer_shell.get_layer_surface(
        spec.surface,
        spec.output,
        spec.layer,
        spec.namespace.to_owned(),
        qh,
        udata,
    );
    ls.set_anchor(spec.anchor);
    ls
}

/// Everything `get_layer_surface` needs beyond the shell and queue handles.
pub struct LayerSpec<'a> {
    pub surface: &'a WlSurface,
    /// `None` means "the compositor picks", which is what we want for a popup
    /// that should follow the caret across outputs.
    pub output: Option<&'a WlOutput>,
    pub layer: Layer,
    pub anchor: Anchor,
    pub namespace: &'a str,
}

pub struct Popup {
    pub surface: WlSurface,
    pub layer_surface: ZwlrLayerSurfaceV1,
    pub state: PopupState,
    pub closed: bool,
}

impl Popup {
    pub fn configure(&mut self, _serial: u32, w: u32, h: u32) {
        self.state.width = w;
        self.state.height = h;
        self.state.pending_ack = true;
    }

    pub fn acknowledge(&mut self, serial: u32) {
        self.layer_surface.ack_configure(serial);
        self.state.pending_ack = false;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_layers() {
        assert!(matches!(parse_layer("overlay"), Ok(Layer::Overlay)));
        assert!(matches!(parse_layer("top"), Ok(Layer::Top)));
        assert!(parse_layer("nonsense").is_err());
        assert!(parse_layer("").is_err());
    }

    #[test]
    fn parse_known_anchors() {
        assert_eq!(parse_anchor("top-left").unwrap(), Anchor::Top | Anchor::Left);
        assert_eq!(parse_anchor("bottom-right").unwrap(), Anchor::Bottom | Anchor::Right);
        assert_eq!(parse_anchor("left").unwrap(), Anchor::Left);
    }

    #[test]
    fn parse_anchor_rejects_unknown() {
        let err = parse_anchor("middle").expect_err("should reject");
        assert!(
            err.to_string().contains("unknown anchor"),
            "error should name the problem, got: {err}"
        );
    }

    #[test]
    fn popup_state_default_is_unconfigured() {
        let s = PopupState::default();
        assert_eq!(s.width, 0);
        assert_eq!(s.height, 0);
        assert!(!s.pending_ack);
    }
}
