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
//! - [`cli`]: command-line flags

pub mod cli;
pub mod config;
pub mod errors;
pub mod fcitx5;
pub mod ipc;
pub mod popup;
pub mod render;
pub mod runtime;
pub mod signals;
pub mod state;
pub mod wayland;

pub use errors::{Error, Result};
