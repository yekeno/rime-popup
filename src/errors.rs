// Minimal stub for Task 0. Replaced wholesale by Task 1 with the full
// `Error` enum + helpers. Kept minimal here so the lib's
// `pub use errors::{Error, Result}` resolves at compile time.

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("not yet implemented: {0}")]
    NotImplemented(String),
}

pub type Result<T> = std::result::Result<T, Error>;
