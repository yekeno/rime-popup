//! Ask niri where the focused text input's caret is.
//!
//! # Why the compositor has to answer this
//!
//! fcitx5's kimpanel API carries a spot rectangle, but for wayland clients
//! that rectangle is empty: the client sends `set_cursor_rectangle` to the
//! compositor over `zwp_text_input_v3`, not to the input method. The compositor
//! is the only process that both sees that request and knows where the focused
//! window sits on its output.
//!
//! rime-popup therefore asks niri over its IPC socket. The request is a plain
//! one-line JSON object and the reply is the output-logical caret rectangle,
//! already offset by the window position. No dependency on `niri-ipc` is
//! needed, which keeps this daemon buildable against a stock niri.
//!
//! When niri cannot answer (X11 clients, an unfocused text input, or no niri
//! at all) the call returns `None` and the caller falls back to the kimpanel
//! rectangle.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::wayland::text_input::CaretRect;

/// How long a single IPC round trip may take before it is abandoned.
///
/// niri answers from the event loop within microseconds; the timeout only
/// protects the D-Bus handler from a wedged compositor.
const TIMEOUT: Duration = Duration::from_millis(100);

/// The request line understood by the patched niri IPC server.
///
/// niri's wire format is the bare serde enum variant, not a tagged object:
/// `"TextInputRect"`, matching every other request its `niri msg` client
/// sends. An object wrapper is rejected with `error parsing request`, which
/// used to make the capability probe fail and silently leave fcitx5 on its
/// own candidate window.
const REQUEST: &[u8] = b"\"TextInputRect\"\n";

/// Cached connection to niri's IPC socket plus the path to reconnect to.
#[derive(Debug)]
pub struct NiriClient {
    path: Option<PathBuf>,
    stream: Option<UnixStream>,
}

impl NiriClient {
    /// Discover niri the same way niri's own clients do: `$NIRI_SOCKET`
    /// first, then the newest `niri.*.sock` in the runtime directory.
    pub fn from_env() -> Self {
        let path = std::env::var_os("NIRI_SOCKET")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(discover_socket);
        if path.is_none() {
            tracing::debug!("no niri IPC socket; caret falls back to kimpanel");
        }
        Self { path, stream: None }
    }

    /// Connect to a specific socket. Used by tests.
    pub fn from_path(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            stream: None,
        }
    }

    /// Whether niri understands the `TextInputRect` request.
    ///
    /// Stock niri replies `{"Err":...}` to the unknown request, which is the
    /// cue to leave the candidate window to fcitx5's classic UI rather than
    /// publishing a kimpanel that can never be positioned.
    pub fn supports_text_input_rect(&mut self) -> bool {
        let Some(path) = self.path.clone() else {
            return false;
        };
        let Some(line) = self.query(&path) else {
            return false;
        };
        serde_json::from_str::<Value>(&line)
            .ok()
            .is_some_and(|reply| reply.get("Ok").is_some())
    }

    /// The focused text input's caret, in output-logical coordinates.
    ///
    /// Returns `None` whenever the compositor has no usable rectangle, which
    /// includes the zero-sized rectangle a client reports before it has drawn
    /// its caret.
    pub fn caret(&mut self) -> Option<CaretRect> {
        self.caret_with(discover_socket)
    }

    /// [`Self::caret`] with socket discovery injected, so tests can simulate a
    /// niri restart (the socket name contains niri's PID and changes when the
    /// compositor restarts).
    fn caret_with(&mut self, discover: impl FnOnce() -> Option<PathBuf>) -> Option<CaretRect> {
        let mut path = self.path.clone()?;
        let line = match self.query(&path) {
            Some(line) => line,
            None => {
                let fresh = discover()?;
                if fresh != path {
                    self.path = Some(fresh.clone());
                    path = fresh;
                }
                self.query(&path)?
            }
        };
        parse_caret(&line)
    }

    fn query(&mut self, path: &Path) -> Option<String> {
        for _ in 0..2 {
            if self.stream.is_none() {
                let stream = UnixStream::connect(path).ok()?;
                stream.set_read_timeout(Some(TIMEOUT)).ok()?;
                stream.set_write_timeout(Some(TIMEOUT)).ok()?;
                self.stream = Some(stream);
            }
            if let Some(line) = self.round_trip() {
                return Some(line);
            }
            // The socket may have been replaced by a niri restart; drop it
            // and retry once with a fresh connection.
            self.stream = None;
        }
        None
    }

    fn round_trip(&mut self) -> Option<String> {
        let stream = self.stream.as_mut()?;
        stream.write_all(REQUEST).ok()?;
        stream.flush().ok()?;

        // Read through a clone so the cached stream stays usable. BufReader
        // reads at most a whole reply per request, and requests are not
        // pipelined, so no bytes are lost between round trips.
        let mut reader = BufReader::new(stream.try_clone().ok()?);
        let mut line = String::new();
        let read = reader.read_line(&mut line).ok()?;
        if read == 0 {
            return None;
        }
        Some(line)
    }
}

impl Default for NiriClient {
    fn default() -> Self {
        Self::from_env()
    }
}

/// Find the newest niri IPC socket in `$XDG_RUNTIME_DIR`.
///
/// niri names the socket `niri.<wayland-display>.<pid>.sock`, so the PID is
/// not guessable. The newest socket is the running instance; stale files from
/// a crashed compositor sort older.
fn discover_socket() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("niri.") && name.ends_with(".sock")
        })
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

/// Extract a caret rectangle from niri's `Reply` JSON.
///
/// `None` covers both an explicit `TextInputRect(null)` (no caret) and any
/// reply that does not carry the field, e.g. an older niri answering
/// `{"Err":"unknown request"}`.
fn parse_caret(line: &str) -> Option<CaretRect> {
    let reply: Value = serde_json::from_str(line).ok()?;
    let rect = reply.get("Ok")?.get("TextInputRect")?;
    if rect.is_null() {
        return None;
    }
    let field = |name: &str| rect.get(name).and_then(Value::as_i64);
    let width = field("width")?;
    let height = field("height")?;
    if width <= 0 || height <= 0 {
        return None;
    }
    Some(CaretRect {
        x: i32::try_from(field("x")?).ok()?,
        y: i32::try_from(field("y")?).ok()?,
        width: i32::try_from(width).ok()?,
        height: i32::try_from(height).ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn parses_a_rectangle() {
        let line = r#"{"Ok":{"TextInputRect":{"x":12,"y":34,"width":2,"height":18}}}"#;
        assert_eq!(
            parse_caret(line),
            Some(CaretRect {
                x: 12,
                y: 34,
                width: 2,
                height: 18,
            })
        );
    }

    #[test]
    fn null_rectangle_is_none() {
        assert_eq!(parse_caret(r#"{"Ok":{"TextInputRect":null}}"#), None);
    }

    #[test]
    fn zero_sized_rectangle_is_none() {
        let line = r#"{"Ok":{"TextInputRect":{"x":0,"y":0,"width":0,"height":0}}}"#;
        assert_eq!(
            parse_caret(line),
            None,
            "zero-sized caret cannot anchor a popup"
        );
    }

    #[test]
    fn error_reply_is_none() {
        assert_eq!(parse_caret(r#"{"Err":"unknown request"}"#), None);
        assert_eq!(parse_caret(r#"{"Ok":"Handled"}"#), None);
        assert_eq!(parse_caret("not json"), None);
    }

    #[test]
    fn queries_a_live_socket() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("niri.wayland-1.4242.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = String::new();
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            reader.read_line(&mut buf).expect("read request");
            assert_eq!(
                buf.trim_end(),
                "\"TextInputRect\"",
                "niri expects the bare enum variant, not an object wrapper"
            );
            stream
                .write_all(
                    b"{\"Ok\":{\"TextInputRect\":{\"x\":7,\"y\":9,\"width\":1,\"height\":16}}}\n",
                )
                .expect("write reply");
        });

        let mut client = NiriClient::from_path(path);
        assert_eq!(
            client.caret(),
            Some(CaretRect {
                x: 7,
                y: 9,
                width: 1,
                height: 16,
            })
        );
        server.join().expect("server thread");
    }

    #[test]
    fn rediscovers_the_socket_after_a_niri_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("niri.wayland-1.9999.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = String::new();
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            reader.read_line(&mut buf).expect("read request");
            stream
                .write_all(
                    b"{\"Ok\":{\"TextInputRect\":{\"x\":1,\"y\":2,\"width\":3,\"height\":4}}}\n",
                )
                .expect("write reply");
        });

        // The stale path has the old PID in it and no longer exists.
        let stale = dir.path().join("niri.wayland-1.1111.sock");
        let mut client = NiriClient::from_path(stale);
        let discovered = path.clone();
        let caret = client.caret_with(|| Some(discovered));
        assert_eq!(
            caret,
            Some(CaretRect {
                x: 1,
                y: 2,
                width: 3,
                height: 4,
            })
        );
        server.join().expect("server thread");
    }

    #[test]
    fn capability_probe_distinguishes_old_niri() {
        for (reply, expected) in [
            (
                "{\"Ok\":{\"TextInputRect\":{\"x\":0,\"y\":0,\"width\":0,\"height\":0}}}\n",
                true,
            ),
            ("{\"Ok\":{\"TextInputRect\":null}}\n", true),
            ("{\"Err\":\"error parsing request\"}\n", false),
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("niri.wayland-1.5150.sock");
            let listener = UnixListener::bind(&path).expect("bind");
            let body = reply.to_string();
            let server_body = body.clone();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = String::new();
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                reader.read_line(&mut buf).expect("read request");
                stream
                    .write_all(server_body.as_bytes())
                    .expect("write reply");
            });
            let mut client = NiriClient::from_path(path);
            assert_eq!(
                client.supports_text_input_rect(),
                expected,
                "reply {body:?}"
            );
            server.join().expect("server thread");
        }
    }

    #[test]
    fn missing_socket_is_not_an_error() {
        let mut client = NiriClient::from_path(PathBuf::from("/nonexistent/niri.sock"));
        // Disable discovery: on a live session it would find the real socket
        // (which is exactly what the daemon should do) and make this test
        // depend on the developer's compositor.
        assert_eq!(client.caret_with(|| None), None);
    }
}
