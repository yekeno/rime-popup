//! Wayland client-side module.
//!
//! Surface the components from one place. Each submodule owns a small piece
//! of state and is initialized from `bind`.

pub mod bind;
pub mod layer_shell;
pub mod registry;
pub mod shm;
pub mod text_input;

use wayland_client::Connection;

use crate::errors::Error;
use crate::Result;

/// Connect to a wayland backend (system default).
///
/// `connect_to_env` fails with `ConnectError` (no compositor, unloadable
/// libwayland, garbage `WAYLAND_SOCKET`), which is a different type from the
/// `WaylandError` the rest of the client produces. Box it into `WaylandConnect`
/// rather than adding a `#[from]` that would collide on `Display`.
pub fn connect() -> Result<Connection> {
    Connection::connect_to_env().map_err(|e| Error::WaylandConnect(Box::new(e)))
}
