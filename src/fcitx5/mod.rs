//! fcitx5 integration.
//!
//! We present ourselves as a kimpanel panel over D-Bus; fcitx5 pushes the
//! candidate list and caret rectangle to us and stops using its own UI.

pub mod events;
pub mod kimpanel;

pub use events::LookupTable;
