//! The layer-shell surface that actually shows the candidate window.
//!
//! One surface is reused for the life of the popup. Its state machine is the
//! usual layer-shell handshake:
//!
//! 1. `create` makes the surface and layer surface, sets anchor/size/margins,
//!    and commits with no buffer. That commit is what asks the compositor for
//!    a `configure`.
//! 2. `configure` must be acknowledged before the first buffer commit.
//! 3. `upload` copies a `tiny-skia` pixmap into a `wl_shm` buffer, attaches
//!    it, damages the whole surface, and commits.
//!
//! Buffers are tracked until the compositor sends `wl_buffer.release`.
//! Destroying a buffer that the compositor still considers in use is a
//! protocol error, so the runtime removes them only on release.

use std::io::Write;
use std::os::fd::AsFd;

use tiny_skia::Pixmap;
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_shm::Format as ShmFormat, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Dispatch, QueueHandle};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1,
};

use crate::wayland::bind::Globals;
use crate::Result;

/// How long a popup stays up before being replaced by a fresh one.
///
/// Kept as one surface; resizing is cheaper than recreating, but a size change
/// still requires a new configure round trip, so the runtime keeps the old
/// contents on screen until the compositor answers.
pub struct Popup {
    surface: WlSurface,
    layer: ZwlrLayerSurfaceV1,
    buffers: Vec<WlBuffer>,
    /// Size we asked the compositor for.
    pub requested: (u32, u32),
    /// Size the compositor configured us at.
    pub actual: (u32, u32),
    pub configured: bool,
}

impl Popup {
    /// Create the surface and ask for the first configure.
    ///
    /// Returns without a buffer attached; the caller must wait for
    /// [`Popup::ack_configure`] before the surface becomes visible.
    pub fn create<D>(
        globals: &Globals,
        qh: &QueueHandle<D>,
        layer: Layer,
        anchor: Anchor,
        size: (u32, u32),
        margin: (i32, i32, i32, i32),
    ) -> Self
    where
        D: Dispatch<WlSurface, ()>
            + Dispatch<ZwlrLayerSurfaceV1, ()>
            + Dispatch<WlShmPool, ()>
            + Dispatch<WlBuffer, ()>
            + 'static,
    {
        let surface = globals.compositor.create_surface(qh, ());
        let layer_surface = globals.layer_shell.get_layer_surface(
            &surface,
            None,
            layer,
            "rime-popup".to_owned(),
            qh,
            (),
        );
        layer_surface.set_anchor(anchor);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
        // -1 tells the compositor this surface is not part of any layout, so
        // the popup never shrinks the usable area of a tiled window.
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_size(size.0, size.1);
        layer_surface.set_margin(margin.0, margin.1, margin.2, margin.3);
        surface.commit();
        tracing::debug!(
            width = size.0,
            height = size.1,
            ?layer,
            "popup surface created; waiting for configure"
        );

        Self {
            surface,
            layer: layer_surface,
            buffers: Vec::new(),
            requested: size,
            actual: (0, 0),
            configured: false,
        }
    }

    /// Ask for a new size without destroying the surface.
    ///
    /// The compositor answers with a fresh `configure`; until then the old
    /// buffer stays on screen.
    pub fn request_size(&mut self, size: (u32, u32), margin: (i32, i32, i32, i32)) {
        self.requested = size;
        self.layer.set_size(size.0, size.1);
        self.layer
            .set_margin(margin.0, margin.1, margin.2, margin.3);
        self.surface.commit();
    }

    /// Update the margins without changing size.
    pub fn set_margin(&mut self, margin: (i32, i32, i32, i32)) {
        self.layer
            .set_margin(margin.0, margin.1, margin.2, margin.3);
        self.surface.commit();
    }

    /// Acknowledge a configure. The compositor's size wins over ours.
    pub fn ack_configure(&mut self, serial: u32, width: u32, height: u32) {
        self.layer.ack_configure(serial);
        self.actual = (width, height);
        self.configured = true;
    }

    /// Upload `canvas` and commit it.
    ///
    /// The pixmap is premultiplied RGBA; `ARGB8888` stores those channels in
    /// B, G, R, A order on little-endian hosts, so the copy swizzles red and
    /// blue while leaving alpha and premultiplication alone.
    pub fn upload<D>(
        &mut self,
        globals: &Globals,
        qh: &QueueHandle<D>,
        canvas: &Pixmap,
    ) -> Result<()>
    where
        D: Dispatch<WlShmPool, ()> + Dispatch<WlBuffer, ()> + 'static,
    {
        let width = canvas.width();
        let height = canvas.height();
        if width == 0 || height == 0 {
            return Ok(());
        }
        let stride = width * 4;
        let len = (stride as usize) * (height as usize);

        let mut file = tempfile::tempfile()?;
        file.set_len(len as u64)?;
        let mut bytes = vec![0u8; len];
        for (src, dst) in canvas.data().chunks_exact(4).zip(bytes.chunks_exact_mut(4)) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = src[3];
        }
        file.write_all(&bytes)?;

        let pool: WlShmPool = globals.shm.create_pool(file.as_fd(), len as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            ShmFormat::Argb8888,
            qh,
            (),
        );
        pool.destroy();

        self.surface.attach(Some(&buffer), 0, 0);
        self.surface
            .damage_buffer(0, 0, width as i32, height as i32);
        self.surface.commit();
        self.buffers.push(buffer);
        tracing::debug!(
            width,
            height,
            buffers = self.buffers.len(),
            "popup frame uploaded"
        );
        Ok(())
    }

    /// Drop a buffer the compositor has released.
    pub fn released(&mut self, buffer: &WlBuffer) {
        self.buffers.retain(|b| b != buffer);
    }

    pub fn has_buffers(&self) -> bool {
        !self.buffers.is_empty()
    }
}

impl Drop for Popup {
    fn drop(&mut self) {
        // Order matters: destroying the surface releases any buffer still
        // attached, and only then do the buffer proxies below get destroyed
        // (Rust drops the remaining fields after this body).
        self.layer.destroy();
        self.surface.destroy();
    }
}
