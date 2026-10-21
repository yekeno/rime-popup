//! fcitx5 integration: DBus proxy pool + event mapper.
//!
//! `proxy` is declared in a later task once `proxy.rs` exists; declaring it
//! here would not compile.

pub mod events;
pub mod proxy;

pub use events::{
    extract_caret, extract_lookup, LookupTable, PageUpdated, CURSOR_PER_CHAR_PROP, CURSOR_POS_PROP,
    CURSOR_RECT_PROP,
};
pub use proxy::Fcitx5Watcher;
