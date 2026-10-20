//! fcitx5 integration: DBus proxy pool + event mapper.
//!
//! `proxy` is declared in a later task once `proxy.rs` exists; declaring it
//! here would not compile.

pub mod events;
pub mod proxy;

pub use events::{extract_lookup, LookupTable, PageUpdated};
pub use proxy::Fcitx5Watcher;
