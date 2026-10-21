//! High-level runtime that owns the calloop loop and the wayland globals.
//!
//! `run` boots a calloop event loop, attaches a `WaylandSource` so pending
//! wayland events are dispatched on our behalf, and runs until SIGINT or
//! SIGTERM arrives, at which point the loop unwinds and the process exits 0.
//!
//! Task 16 is deliberately a skeleton: the loop starts, wayland events flow,
//! and the process exits cleanly on a signal. Binding the globals we need out
//! of `wl_registry` is Task 17's job; SIGHUP reload and SIGUSR1 state dump
//! ride on top of the same signal source in Task 18, as does the fcitx5 Dbus
//! bridge.
//!
//! # Why `ObjectData` rather than `Dispatch`
//!
//! `wayland-client` 0.31 removed `EventQueue::set_dispatcher`. Global state
//! now receives events by implementing [`wayland_client::backend::ObjectData`]
//! and passing an `Arc` of itself as the userdata of the constructed proxy
//! (see `list_globals_no_dispatch.rs` in the crate's examples). `Dispatch` is
//! still available and is arguably nicer for a larger daemon, but `ObjectData`
//! keeps Task 16 to a single struct with no trait-plumbing, and swapping in
//! `Dispatch` later touches only the event-routing, not the loop wiring.
//!
//! # Why signals are installed explicitly
//!
//! calloop 0.13's `EventLoop::try_new` no longer registers SIGINT/SIGTERM —
//! older versions did, so code written against 0.12 silently loses graceful
//! shutdown on upgrade. Without the explicit `Signals` source below the daemon
//! dies on the default disposition and exits 143 instead of 0.
//!
//! Two traps worth naming: the module is `calloop::signals`, gated behind the
//! `signals` feature; and `calloop::LoopSignal` is **not** it — that type is an
//! async waker for `block_on` and has nothing to do with Unix signals. The
//! feature pulls in `nix`, which is already in the dependency tree, so this
//! adds no new crate to the build.

use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};

use calloop::signals::{Signal, Signals};
use calloop::EventLoop;
use wayland_client::backend::{self, Backend};
use wayland_client::protocol::{
    wl_compositor::{Event as WlCompositorEvent, WlCompositor},
    wl_display, wl_registry,
    wl_registry::WlRegistry,
    wl_seat::{Capability as SeatCapability, Event as WlSeatEvent, WlSeat},
    wl_shm::{Event as WlShmEvent, WlShm},
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};

use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::{
    Event as TextInputManagerEvent, ZwpTextInputManagerV3,
};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    Event as TextInputEvent, ZwpTextInputV3,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{
    Event as LayerShellEvent, ZwlrLayerShellV1,
};

use crate::config::Config;
use crate::state::State;
use crate::wayland::bind::{self, Globals};
use crate::wayland::registry::Needed;
use crate::wayland::text_input::{LatestCaret, WlSurfaceId};
use crate::Result;

/// Globals advertised by the compositor, in advertisement order.
pub type GlobalList = Vec<(u32, String, u32)>;

/// Registry userdata: parses `wl_registry` events and forwards the
/// advertisements into a shared sink.
struct RegistryData {
    conn: Arc<Connection>,
    sink: Arc<Mutex<GlobalList>>,
}

impl backend::ObjectData for RegistryData {
    fn event(
        self: Arc<Self>,
        _: &Backend,
        msg: backend::protocol::Message<backend::ObjectId, OwnedFd>,
    ) -> Option<Arc<dyn backend::ObjectData>> {
        let (_registry, event) = wl_registry::WlRegistry::parse_event(&self.conn, msg).ok()?;
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if let Ok(mut sink) = self.sink.lock() {
                sink.push((name, interface.to_string(), version));
            }
        }
        None
    }

    fn destroyed(&self, _: backend::ObjectId) {
        // A `wl_registry` cannot be destroyed by the compositor; there is no
        // recovery path if we ever get here.
        unreachable!("wl_registry was destroyed");
    }
}

/// Advertise `wl_registry` and synchronously collect every global.
///
/// `roundtrip` blocks until the compositor has replied with its full global
/// list, which is exactly what we want before the event loop takes over: the
/// first iteration then already has every handle the daemon needs.
///
/// The registry proxy is returned alongside the list because binding is a
/// request *on* it — dropping it here would make [`crate::wayland::bind`]
/// impossible.
pub fn collect_globals(conn: &Arc<Connection>) -> Result<(WlRegistry, GlobalList)> {
    let display = conn.display();
    let sink: Arc<Mutex<GlobalList>> = Arc::new(Mutex::new(GlobalList::new()));
    let data = Arc::new(RegistryData {
        conn: Arc::clone(conn),
        sink: Arc::clone(&sink),
    });

    // `get_registry` is a constructor: it creates the proxy, so the request
    // carries the userdata rather than being a plain `send_request`.
    let registry: WlRegistry = display
        .send_constructor(wl_display::Request::GetRegistry {}, data)
        .map_err(|e| crate::Error::Internal(format!("get_registry: {e}")))?;

    conn.roundtrip()
        .map_err(|e| crate::Error::Internal(format!("roundtrip: {e}")))?;

    let globals = sink.lock().map(|g| g.clone()).unwrap_or_default();
    Ok((registry, globals))
}

/// Loop-owned state, reachable from every calloop dispatcher via `&mut D`.
pub struct LoopData {
    pub state: State,
    pub needed: Needed,
    /// Everything the compositor advertised, kept for diagnostics.
    pub advertised: GlobalList,
    /// Live handles to the five globals we need, bound during startup.
    pub globals: Option<Globals>,
    /// The per-seat text input, created once `wl_seat` reports a keyboard.
    pub text_input: Option<ZwpTextInputV3>,
    /// Seat name from `wl_seat.name`; logged for diagnostics.
    pub seat_name: Option<String>,
    /// Most recent caret rectangle, positioned by the popup.
    pub caret: LatestCaret,
}

impl LoopData {
    /// Create the `zwp_text_input_v3` for this seat, once.
    ///
    /// The manager is per-connection, but text input is per-seat: the v3
    /// request takes the `wl_seat` itself, not a version number as v2 did.
    fn ensure_text_input(&mut self, qh: &QueueHandle<Self>) {
        if self.text_input.is_some() {
            return;
        }
        let Some(g) = self.globals.as_ref() else {
            tracing::warn!("seat capabilities arrived before globals were bound");
            return;
        };
        self.text_input = Some(g.text_input_manager.get_text_input(&g.seat, qh, ()));
        tracing::info!(seat = ?self.seat_name, "created zwp_text_input_v3");
    }
}

impl Default for LoopData {
    fn default() -> Self {
        LoopData {
            state: State::new(),
            needed: Needed::default(),
            advertised: GlobalList::new(),
            globals: None,
            text_input: None,
            seat_name: None,
            caret: LatestCaret::default(),
        }
    }
}

// --- Protocol event routing -------------------------------------------------
//
// `WlRegistry::bind` is the only way to bind a global in wayland-client 0.31,
// and it requires a `Dispatch` impl for the queue's data type on every
// interface it binds. These five impls are that requirement; the ones that
// carry real behaviour (seat, text input) mutate `LoopData` directly, which
// is why no locking is needed anywhere on this path.

impl Dispatch<WlCompositor, ()> for LoopData {
    // `wl_compositor` has no events; the impl exists only because
    // `WlRegistry::bind` demands one per interface it binds.
    fn event(
        _: &mut Self,
        _: &WlCompositor,
        _: WlCompositorEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShm, ()> for LoopData {
    fn event(
        _: &mut Self,
        _: &WlShm,
        event: WlShmEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // `ShmPool` renders ARGB8888 and converts to RGBA8 itself, which every
        // compositor is required to support, so enumerating formats buys us
        // nothing today.
        if matches!(event, WlShmEvent::Format { .. }) {
            tracing::trace!("wl_shm advertised a format (ignored)");
        }
    }
}

impl Dispatch<WlSeat, ()> for LoopData {
    fn event(
        state: &mut Self,
        _: &WlSeat,
        event: WlSeatEvent,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            WlSeatEvent::Name { name } => {
                tracing::info!(seat = %name, "seat identified");
                state.seat_name = Some(name);
            }
            WlSeatEvent::Capabilities { capabilities } => {
                // Keyboard is the capability an input method actually needs;
                // pointer-only seats are irrelevant to a candidate popup.
                let bits = match capabilities {
                    WEnum::Value(c) => u32::from(c),
                    // A capability bit we do not know about. Keep the raw
                    // value so a future compositor extension still works.
                    WEnum::Unknown(raw) => raw,
                };
                if bits & u32::from(SeatCapability::Keyboard) != 0 {
                    state.ensure_text_input(qh);
                } else {
                    tracing::info!("seat lost keyboard capability");
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpTextInputManagerV3, ()> for LoopData {
    fn event(
        _: &mut Self,
        _: &ZwpTextInputManagerV3,
        _: TextInputManagerEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrLayerShellV1, ()> for LoopData {
    fn event(
        state: &mut Self,
        _: &ZwlrLayerShellV1,
        event: LayerShellEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        tracing::warn!(?event, "layer shell event; popup cannot be shown");
        if let Some(ic) = state.state.latest_focus().cloned() {
            state.state.focus_out(&ic);
        }
    }
}

impl Dispatch<ZwpTextInputV3, ()> for LoopData {
    fn event(
        state: &mut Self,
        _: &ZwpTextInputV3,
        event: TextInputEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Only `enter`/`leave` matter here. This protocol revision does NOT
        // report the caret to the input method — `set_cursor_rectangle` is a
        // request *we* send, and `done` carries only a serial. The caret
        // rectangle therefore has to come from fcitx5's DBus interface, not
        // from wayland; see `fcitx5::CaretSource`.
        match event {
            TextInputEvent::Enter { surface } => {
                state
                    .caret
                    .focus_enter(WlSurfaceId(surface.id().protocol_id()));
            }
            TextInputEvent::Leave { surface } => {
                state
                    .caret
                    .focus_leave(WlSurfaceId(surface.id().protocol_id()));
            }
            _ => {}
        }
    }
}

/// Signals that ask the daemon to shut down.
///
/// Deliberately narrow in Task 16. Task 18 adds SIGHUP (reload config) and
/// SIGUSR1 (dump state) to the same source and branches on `Event::signal()`
/// instead of stopping the loop.
const SHUTDOWN_SIGNALS: &[Signal] = &[Signal::SIGINT, Signal::SIGTERM];

/// Trap the shutdown signals and turn them into a loop stop.
///
/// calloop masks these on the loop thread and reports them through a
/// `signalfd`, so the callback runs on the event loop rather than on a real
/// async-signal context — which is why it may log and touch state.
///
/// `stopper` comes from [`EventLoop::get_signal`]; `LoopHandle` has no `stop`
/// of its own, and `EventLoop` is not reachable from inside a callback.
fn install_signal_handlers(
    handle: &calloop::LoopHandle<'_, LoopData>,
    stopper: calloop::LoopSignal,
) -> Result<()> {
    let signals = Signals::new(SHUTDOWN_SIGNALS)
        .map_err(|e| crate::Error::Internal(format!("signal registration: {e}")))?;
    handle
        .insert_source(signals, move |_, _, _| stopper.stop())
        .map_err(|e| crate::Error::Internal(format!("signal source insert: {e}")))?;
    tracing::info!(signals = ?SHUTDOWN_SIGNALS, "installed shutdown signal handlers");
    Ok(())
}

pub fn run(cfg: Config) -> Result<()> {
    let _ = cfg; // forward-compat: Task 18 wires config into reload

    let conn = Arc::new(crate::wayland::connect()?);
    let (registry, advertised) = collect_globals(&conn)?;

    let mut needed = Needed::default();
    for (name, interface, version) in &advertised {
        crate::wayland::registry::record(&mut needed, *name, interface, *version);
    }
    tracing::info!(
        advertised = advertised.len(),
        satisfied = needed.is_satisfied(),
        "enumerated wayland globals"
    );
    if !needed.is_satisfied() {
        let missing: Vec<&str> = crate::wayland::registry::REQUIRED
            .iter()
            .copied()
            .filter(|i| needed.version_of(i).is_none())
            .collect();
        return Err(crate::Error::MissingGlobal(missing.join(", ")));
    }

    let mut evl: EventLoop<'static, LoopData> =
        EventLoop::try_new().map_err(|e| crate::Error::Internal(format!("calloop new: {e}")))?;

    let queue = Arc::clone(&conn).new_event_queue();
    let qh = queue.handle();

    // Bind before the loop starts. `wl_seat` has to exist before we can ask
    // for a text input, and the seat's capability event arrives on the very
    // first dispatch — after this returns.
    let globals = bind::bind_all(&registry, &needed, &qh)?;
    tracing::info!(?globals, "bound required globals");

    // `WaylandSource` takes an owned `Connection`; at this point the only
    // outstanding ref is our own `Arc`, so unwrap normally succeeds. The
    // fallback clones rather than unwrapping so a lingering ref (e.g. from a
    // future dispatcher holding a clone) degrades instead of panicking.
    let owned_conn = Arc::try_unwrap(conn).unwrap_or_else(|c| (*c).clone());
    let _token = calloop_wayland_source::WaylandSource::new(owned_conn, queue)
        .insert(evl.handle())
        .map_err(|e| crate::Error::Internal(format!("wayland source insert: {e}")))?;

    install_signal_handlers(&evl.handle(), evl.get_signal())?;

    tracing::info!("entering calloop event loop");

    let mut data = LoopData {
        needed,
        advertised,
        globals: Some(globals),
        ..LoopData::default()
    };
    evl.run(None, &mut data, |data| {
        // Placeholder: Task 18 adds fcitx5 IPC and signal-driven reload; the
        // popup itself is painted from `popup::decide_and_paint` once a
        // surface exists to draw on.
        let _ = data;
    })
    .map_err(|e| crate::Error::Internal(format!("calloop run: {e}")))?;

    tracing::info!("event loop exited cleanly");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wayland::registry;

    #[test]
    fn loop_data_starts_unsatisfied() {
        assert!(!LoopData::default().needed.is_satisfied());
    }

    #[test]
    fn globals_convert_into_needed() {
        let mut n = registry::Needed::default();
        for (name, iface, version) in [
            (1u32, "wl_compositor", 4u32),
            (2, "wl_shm", 1),
            (3, "wl_seat", 8),
            (4, "zwp_text_input_manager_v3", 1),
            (5, "zwlr_layer_shell_v1", 4),
        ] {
            assert!(registry::record(&mut n, name, iface, version));
        }
        assert!(n.is_satisfied());
    }

    // Requires a live wayland session; ignored so the suite stays hermetic.
    #[ignore = "needs a real wayland session; run manually under niri"]
    #[test]
    fn runtime_starts_and_exits_cleanly() {
        let _ = run(crate::config::Config::default_built());
    }
}
