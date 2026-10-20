//! fcitx5 integration: DBus proxy pool + event mapper.
//!
//! `proxy` is declared in a later task once `proxy.rs` exists; declaring it
//! here would not compile.

pub mod events;

pub use events::{extract_lookup, LookupTable, PageUpdated};
