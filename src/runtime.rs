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

// `signals` below is our own module, so calloop's event type needs a
// distinct name to stay reachable.
use calloop::signals::{Event as SignalEvent, Signals};
use calloop::EventLoop;
use wayland_client::backend::{self, Backend};
use wayland_client::protocol::{
    wl_buffer::{Event as WlBufferEvent, WlBuffer},
    wl_compositor::{Event as WlCompositorEvent, WlCompositor},
    wl_display, wl_registry,
    wl_registry::WlRegistry,
    wl_seat::{Capability as SeatCapability, Event as WlSeatEvent, WlSeat},
    wl_shm::{Event as WlShmEvent, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};

use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::{
    Event as TextInputManagerEvent, ZwpTextInputManagerV3,
};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    Event as TextInputEvent, ZwpTextInputV3,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{
    Event as LayerShellEvent, Layer, ZwlrLayerShellV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    Event as LayerSurfaceEvent, ZwlrLayerSurfaceV1,
};

use crate::config::Config;
use crate::ipc::{self, DaemonToWayland};
use crate::popup;
use crate::render::text::FontFace;
use crate::signals::{self, DaemonSignal, Reloadable};
use crate::state::State;
use crate::wayland::bind::{self, Globals};
use crate::wayland::layer_shell::{parse_anchor, parse_layer};
use crate::wayland::popup::Popup;
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
    /// Config in force. Replaced wholesale on SIGHUP.
    pub config: Config,
    /// Queue handle, kept so callbacks can create proxies.
    pub qh: Option<QueueHandle<Self>>,
    /// The CJK font, loaded once at startup and on reload. `None` means text
    /// is skipped but the popup still paints its background.
    pub font: Option<FontFace>,
    /// The one popup surface, created lazily on the first candidate list.
    pub popup: Option<Popup>,
    /// Most recently painted frame, re-uploaded after each configure.
    pub last_frame: Option<popup::PopupFrame>,
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

    /// Adopt a freshly reloaded config.
    ///
    /// Only the live loop state is touched. Globals, seats and text input were
    /// negotiated with the compositor at startup and cannot be renegotiated
    /// without reconnecting, so a reload covers everything the daemon reads on
    /// its own — render and filter settings — and leaves the wire alone. That
    /// split is why this is a method rather than a wholesale swap: a config
    /// naming a different surface or seat must not tear down a working
    /// connection.
    fn apply_reloaded_config(&mut self, cfg: &Config) {
        self.config = cfg.clone();
        self.font = load_font(&cfg.render);
        tracing::info!(
            font_size = cfg.render.font_size,
            log_level = cfg.general.log,
            ignore_apps = cfg.filter.ignore_app_ids.len(),
            "applied reloaded config to loop state"
        );
    }

    /// Log a snapshot of what the daemon currently believes.
    ///
    /// This is the first thing to ask for when a popup does not appear: it
    /// distinguishes "never got a caret" from "got a caret but no candidates"
    /// from "no input context at all", which are three very different bugs
    /// that otherwise look identical from outside.
    fn dump_state(&self) {
        tracing::info!(
            seat = ?self.seat_name,
            globals_bound = self.globals.is_some(),
            text_input_created = self.text_input.is_some(),
            advertised_globals = self.advertised.len(),
            focused_surface = ?self.caret.focus,
            caret_rect = ?self.caret.rect,
            focused_ic = ?self.state.latest_focus(),
            "SIGUSR1 state dump"
        );
    }

    /// Fold one message from the fcitx5 thread into the loop state.
    ///
    /// Every message ends with a repaint attempt: candidate and caret updates
    /// arrive independently and in either order, so the only reliable place to
    /// decide whether a frame is drawable is after each one.
    fn handle_daemon_msg(&mut self, msg: DaemonToWayland) {
        match msg {
            DaemonToWayland::FocusIn(ic) => {
                self.state.insert_ic(ic.clone());
                self.state.focus_in(&ic);
            }
            DaemonToWayland::FocusOut(ic) => self.state.focus_out(&ic),
            DaemonToWayland::Removed(ic) => self.state.remove_ic(&ic),
            DaemonToWayland::Commit(ic) => {
                self.state
                    .set_lookup(&ic, crate::fcitx5::events::LookupTable::default());
            }
            DaemonToWayland::LookupTable { ic, table } => {
                self.state.insert_ic(ic.clone());
                self.state.focus_in(&ic);
                self.state.set_lookup(&ic, table);
            }
            DaemonToWayland::Caret { ic, rect } => {
                self.state.insert_ic(ic.clone());
                self.state.focus_in(&ic);
                self.caret.set_rect(rect);
            }
        }
        self.refresh_popup();
    }

    /// The frame the current state would paint, if any.
    fn current_frame(&self) -> Option<popup::PopupFrame> {
        popup::decide_and_paint(&self.state, &self.caret, &self.config, self.font.as_ref())
    }

    /// Margins placing the popup just below the caret.
    ///
    /// The popup is anchored top-left, so only the top and left margins are
    /// meaningful. Coordinates come from the client's IM module, which for
    /// wayland clients are toplevel-relative; niri's focused column sits at
    /// the output's left edge, which makes the two spaces coincide in the
    /// common case.
    fn popup_margin(&self) -> (i32, i32, i32, i32) {
        let Some(rect) = self.caret.rect else {
            return (0, 0, 0, 0);
        };
        let (x, y, h) = (rect.x, rect.y, rect.height);
        let top = y
            .saturating_add(h)
            .saturating_add(i32::try_from(self.config.render.vertical_offset).unwrap_or(i32::MAX));
        (top, 0, 0, x)
    }

    /// Recompute the popup and show or hide it.
    fn refresh_popup(&mut self) {
        let Some(frame) = self.current_frame() else {
            self.hide_popup();
            return;
        };
        tracing::debug!(
            width = frame.canvas.width(),
            height = frame.canvas.height(),
            "popup frame ready"
        );
        self.last_frame = Some(frame);
        self.present();
    }

    /// Push the latest frame to the compositor, creating or resizing the
    /// surface as needed.
    fn present(&mut self) {
        // Moved out so `&frame` does not borrow `self` while `self.popup` is
        // borrowed mutably.
        let Some(frame) = self.last_frame.take() else {
            return;
        };
        self.present_frame(&frame);
        self.last_frame = Some(frame);
    }

    fn present_frame(&mut self, frame: &popup::PopupFrame) {
        let Some(globals) = self.globals.as_ref() else {
            return;
        };
        let Some(qh) = self.qh.clone() else {
            return;
        };
        let size = (frame.metrics.width.max(1), frame.metrics.height.max(1));
        let margin = self.popup_margin();

        if self.popup.is_none() {
            let layer = parse_layer(&self.config.wayland.layer).unwrap_or(Layer::Overlay);
            let anchor = parse_anchor(&self.config.wayland.anchor)
                .unwrap_or(wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor::Top
                    | wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor::Left);
            self.popup = Some(Popup::create(globals, &qh, layer, anchor, size, margin));
            // The buffer is attached from the configure callback.
            return;
        }

        let popup = self.popup.as_mut().expect("created above");
        if popup.requested != size {
            popup.request_size(size, margin);
            return;
        }
        popup.set_margin(margin);
        // Only paint once the compositor has acknowledged this exact size.
        // Uploading a resized buffer before its configure would show the new
        // pixels at the old geometry for a frame.
        if popup.configured && popup.actual == size {
            if let Err(e) = popup.upload(globals, &qh, &frame.canvas) {
                tracing::warn!(error = %e, "failed to upload popup frame");
            }
        }
    }

    fn hide_popup(&mut self) {
        if self.popup.take().is_some() {
            tracing::debug!("popup hidden");
        }
        self.last_frame = None;
    }
}

/// Load the CJK font, logging rather than aborting when it is unavailable.
fn load_font(cfg: &crate::config::RenderConfig) -> Option<FontFace> {
    let path = std::path::Path::new(&cfg.font_fallback_cjk);
    match FontFace::load(path) {
        Ok(font) => Some(font),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "candidate font unavailable; painting without text"
            );
            None
        }
    }
}

/// Start the fcitx5 kimpanel bridge on its own thread.
///
/// zbus needs a tokio reactor; the wayland side is calloop. Crossing that
/// boundary on a dedicated thread keeps either event loop from having to know
/// about the other — all they share is the [`crate::ipc`] channel.
fn spawn_fcitx5_bridge(tx: ipc::Sender<DaemonToWayland>) -> Result<()> {
    std::thread::Builder::new()
        .name("fcitx5-dbus".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!(error = %e, "failed to build tokio runtime for fcitx5");
                    return;
                }
            };
            if let Err(e) = runtime.block_on(crate::fcitx5::kimpanel::publish(tx)) {
                tracing::error!(error = %e, "fcitx5 kimpanel bridge stopped");
            }
        })
        .map_err(|e| crate::Error::Internal(format!("spawn fcitx5 bridge: {e}")))?;
    Ok(())
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
            config: Config::default_built(),
            qh: None,
            font: None,
            popup: None,
            last_frame: None,
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

impl Dispatch<WlSurface, ()> for LoopData {
    // `wl_surface` events (enter/leave/scale) do not affect a popup that is
    // positioned from the caret rectangle, so they are ignored.
    fn event(
        _: &mut Self,
        _: &WlSurface,
        _: <WlSurface as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShmPool, ()> for LoopData {
    // `wl_shm_pool` has no events; the impl exists so `create_pool` can bind.
    fn event(
        _: &mut Self,
        _: &WlShmPool,
        _: <WlShmPool as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlBuffer, ()> for LoopData {
    fn event(
        state: &mut Self,
        buffer: &WlBuffer,
        event: WlBufferEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let WlBufferEvent::Release = event {
            if let Some(popup) = state.popup.as_mut() {
                popup.released(buffer);
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for LoopData {
    fn event(
        state: &mut Self,
        _: &ZwlrLayerSurfaceV1,
        event: LayerSurfaceEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            LayerSurfaceEvent::Configure {
                serial,
                width,
                height,
            } => {
                tracing::debug!(serial, width, height, "popup configured");
                if let Some(popup) = state.popup.as_mut() {
                    popup.ack_configure(serial, width, height);
                }
                // Safe to upload now: the configure was acknowledged first.
                state.present();
            }
            LayerSurfaceEvent::Closed => {
                tracing::debug!("compositor closed the popup layer surface");
                state.popup = None;
                state.last_frame = None;
            }
            _ => {}
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
    mut reloadable: Reloadable,
) -> Result<()> {
    let signals = Signals::new(signals::WATCHED)
        .map_err(|e| crate::Error::Internal(format!("signal registration: {e}")))?;
    handle
        .insert_source(
            signals,
            // calloop hands the callback `(event, source_data, loop_data)` —
            // in that order, with the loop data last.
            // calloop hands the callback `(event, source_data, loop_data)` —
            // in that order. The source data for `Signals` is `()`.
            move |event: SignalEvent, _: &mut (), data: &mut LoopData| {
                let signal = event.signal();
                match signals::classify(signal) {
                    DaemonSignal::Shutdown => {
                        tracing::info!(?signal, "shutdown signal received");
                        stopper.stop();
                    }
                    DaemonSignal::Reload => match reloadable.reload() {
                        Ok(true) => {
                            tracing::info!(path = %reloadable.path().display(), "config reloaded");
                            data.apply_reloaded_config(reloadable.get());
                        }
                        // The failure is already logged with its cause by
                        // `reload`; repeating it here would only add noise.
                        Ok(false) => tracing::debug!("SIGHUP: config unchanged"),
                        Err(e) => tracing::error!(error = %e, "SIGHUP reload failed"),
                    },
                    DaemonSignal::Dump => data.dump_state(),
                }
            },
        )
        .map_err(|e| crate::Error::Internal(format!("signal source insert: {e}")))?;
    tracing::info!(signals = ?signals::WATCHED, "installed signal handlers");
    Ok(())
}

pub fn run(reloadable: Reloadable) -> Result<()> {
    // Cloned before `reloadable` moves into the signal handler; the handler
    // owns the authoritative copy from then on.
    let data_cfg = reloadable.get().clone();
    tracing::info!(
        config_path = %reloadable.path().display(),
        "starting with this config"
    );

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

    install_signal_handlers(&evl.handle(), evl.get_signal(), reloadable)?;

    // fcitx5 talks to us over D-Bus on a tokio thread; the candidate and caret
    // updates it produces land here as calloop events.
    let (tx, chan) = ipc::make_pair();
    spawn_fcitx5_bridge(tx)?;
    evl.handle()
        .insert_source(chan, |event, _source, data: &mut LoopData| match event {
            calloop::channel::Event::Msg(msg) => data.handle_daemon_msg(msg),
            calloop::channel::Event::Closed => {
                tracing::warn!("fcitx5 bridge channel closed");
            }
        })
        .map_err(|e| crate::Error::Internal(format!("ipc channel insert: {e}")))?;

    tracing::info!("entering calloop event loop");

    let mut data = LoopData {
        needed,
        advertised,
        globals: Some(globals),
        config: data_cfg.clone(),
        qh: Some(qh.clone()),
        font: load_font(&data_cfg.render),
        ..LoopData::default()
    };
    evl.run(None, &mut data, |_data| {
        // Everything is driven by callbacks: wayland events, the fcitx5 IPC
        // channel, and signals. There is no idle work to do here.
    })
    .map_err(|e| crate::Error::Internal(format!("calloop run: {e}")))?;

    tracing::info!("event loop exited cleanly");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::IcId;
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
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("rime-popup.toml");
        let _ = run(Reloadable::new(Config::default_built(), path, true));
    }

    #[test]
    fn dump_state_tolerates_a_bare_loop() {
        // Nothing bound, no seat, no caret: the dump must still run. This is
        // the state a user is in when SIGUSR1 is the first thing they try.
        let d = LoopData::default();
        d.dump_state();
    }

    #[test]
    fn candidates_and_caret_produce_a_frame() {
        let mut d = LoopData::default();
        let ic: IcId = std::sync::Arc::from("/kimpanel");
        d.handle_daemon_msg(DaemonToWayland::LookupTable {
            ic: ic.clone(),
            table: crate::fcitx5::events::LookupTable {
                candidates: vec!["你".into(), "泥".into()],
                labels: vec!["1.".into(), "2.".into()],
                cursor_pos: 0,
                page_size: 0,
                aux_up: None,
                aux_down: Some("1/3".into()),
                preedit: None,
            },
        });
        assert!(d.current_frame().is_none(), "no caret yet");
        d.handle_daemon_msg(DaemonToWayland::Caret {
            ic,
            rect: crate::wayland::text_input::CaretRect {
                x: 12,
                y: 30,
                width: 1,
                height: 18,
            },
        });
        let frame = d.current_frame().expect("frame");
        assert!(frame.canvas.width() > 0);
    }

    #[test]
    fn an_empty_lookup_hides_the_popup() {
        let mut d = LoopData::default();
        let ic: IcId = std::sync::Arc::from("/kimpanel");
        d.handle_daemon_msg(DaemonToWayland::Caret {
            ic: ic.clone(),
            rect: crate::wayland::text_input::CaretRect {
                x: 1,
                y: 2,
                width: 1,
                height: 16,
            },
        });
        d.handle_daemon_msg(DaemonToWayland::LookupTable {
            ic,
            table: crate::fcitx5::events::LookupTable::default(),
        });
        assert!(d.current_frame().is_none());
    }

    #[test]
    fn popup_margin_sits_below_the_caret() {
        let mut d = LoopData::default();
        let ic: IcId = std::sync::Arc::from("/kimpanel");
        d.handle_daemon_msg(DaemonToWayland::Caret {
            ic,
            rect: crate::wayland::text_input::CaretRect {
                x: 200,
                y: 300,
                width: 2,
                height: 24,
            },
        });
        let offset = d.config.render.vertical_offset as i32;
        assert_eq!(d.popup_margin(), (300 + 24 + offset, 0, 0, 200));
    }

    #[test]
    fn popup_margin_without_a_caret_uses_the_origin() {
        let d = LoopData::default();
        assert_eq!(d.popup_margin(), (0, 0, 0, 0));
    }

    #[test]
    fn refresh_without_globals_does_not_create_a_surface() {
        let mut d = LoopData::default();
        let ic: IcId = std::sync::Arc::from("/kimpanel");
        d.handle_daemon_msg(DaemonToWayland::LookupTable {
            ic: ic.clone(),
            table: crate::fcitx5::events::LookupTable {
                candidates: vec!["你".into()],
                ..Default::default()
            },
        });
        d.handle_daemon_msg(DaemonToWayland::Caret {
            ic,
            rect: crate::wayland::text_input::CaretRect {
                x: 1,
                y: 2,
                width: 1,
                height: 16,
            },
        });
        assert!(d.popup.is_none());
        assert!(d.last_frame.is_some());
        d.handle_daemon_msg(DaemonToWayland::Commit(std::sync::Arc::from("/kimpanel")));
        assert!(d.last_frame.is_none());
    }

    #[test]
    fn reloaded_config_replaces_the_one_in_force() {
        let mut d = LoopData::default();
        let before = d.config.render.font_size;
        let mut fresh = Config::default_built();
        fresh.render.font_size = before + 7;
        d.apply_reloaded_config(&fresh);
        assert_eq!(d.config.render.font_size, before + 7);
    }
}
