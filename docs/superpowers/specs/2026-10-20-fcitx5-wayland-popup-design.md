# Rime Popup: fcitx5 → Wayland Candidate Popup Daemon

| Field | Value |
|---|---|
| Date | 2026-10-20 |
| Status | Approved (user, in session) |
| Author | brainstorming session |
| Target | niri 26.04 + fcitx5 5.0.14 + Ubuntu 22.04 |

## 1. Problem

emacs-rime renders its own candidate window inside Emacs and follows the cursor
exactly. Native apps under niri (Chromium, alacritty, foot, …) currently do
**not** show the candidate popup at all when fcitx5 is the input backend.
emacs-rime is unaffected because it owns its UI; the fcitx5 popup path on
Wayland is broken for these apps.

## 2. Goal

Build a single Rust daemon (`rime-popup`) that:

1. Subscribes to **existing** fcitx5 `InputContext1` signals (does not create
   its own IC and does not consume key events).
2. Tracks the focused wayland surface and caret rectangle via
   `zwp_text_input_v3`.
3. Renders a candidate popup via `zwlr_layer_shell_v1`, positioned by caret
   rect, drawn with `swash` + `tiny-skia` into a shm buffer.
4. Hits ≥80% test coverage and ships as a release-mode static-ish single
   binary invoked by systemd `--user`.

### Out of scope (v1)

- emoji picker, soft keyboard, multiple seats, custom protocol for apps.
- Replacing or stopping fcitx5 (it stays the source of truth).
- DRM/KMS integration — we are a wayland client only.

## 3. Architecture

```
                       ┌─────────────────────────────────────────┐
                       │ systemd --user (Restart=always)         │
                       │   /usr/local/bin/rime-popup             │
                       └─────────────────────────────────────────┘
                                                │
       ┌────────────────────────┐               │ threads
       │ tokio runtime (1 thread + multi worker)│
       │  └─ zbus proxy pool                    │
       │      ├─ org.fcitx.Fcitx5               │
       │      ├─ ObjectManager ItemsChanged     │
       │      └─ /InputContext/{N}:             │
       │           InputContextUpdatedSignal    │
       │           CommitStringSignal           │
       └────────────────────────┘               │
                                                ▼
                ┌──────────────────────────────────────────┐
                │ calloop main thread (wayland)            │
                │  ├─ wl_display roundtrip                  │
                │  ├─ zwp_text_input_v3     ← enter /         │
                │  │   cursor_rect / commit  caret rect     │
                │  ├─ zwlr_layer_shell_v1   ← popup surface │
                │  └─ wl_shm + wl_compositor  frame callback│
                └──────────────────────────────────────────┘
                       ▲                       ▲
                       │                       │ calloop::channel
       ┌────────────────────────┐               │
       │ tokio runtime (1 thread + multi worker)│
       │  ├─ zbus proxy pool                    │
       │  │   ├─ org.fcitx.Fcitx5               │
       │  │   ├─ ObjectManager ItemsChanged     │
       │  │   └─ /InputContext/{N}:             │
       │  │        InputContextUpdatedSignal    │
       │  │        CommitStringSignal           │
       │  └─ crossbeam_forwarder                │
       └────────────────────────┘               │
                                                │
                       zbus signals forwarded ─┘
                       from tokio into calloop loop
```

### 3.1 Component map

| Component | Responsibility |
|---|---|
| `main.rs` | parse CLI, init tracing, spawn tokio + calloop, signal handler |
| `config.rs` | load `$XDG_CONFIG_HOME/rime-popup/config.toml`, CLI overrides |
| `fcitx5/proxy.rs` | zbus proxies for `IC.M1` and IC children |
| `fcitx5/events.rs` | `InputContextUpdatedSignal` → `IMState { candidates, page, cursor_pos, preedit, aux }` |
| `wayland/mod.rs` | registry bind, seat listener, dispatcher to text_input/layer_shell/shm |
| `wayland/text_input.rs` | wrap `zwp_text_input_v3`, emit `caret_rect` events |
| `wayland/layer_shell.rs` | create/move/resize `zwlr_layer_surface_v1`, ack configure |
| `wayland/shm.rs` | pool of reusable `wl_buffer` sized by §5.1 |
| `render/layout.rs` | per-candidate geometry + per-line metrics |
| `render/paint.rs` | swash shaping → glyph runs → tiny-skia paint → premultiplied RGBA |
| `ipc/to_wayland.rs` | `calloop::channel::Sender<DaemonToWayland>` (tokio → calloop bridge) |
| `state.rs` | per-IC `IMState`, global `latest_focus: Option<WlSurface>` |
| `popup.rs` | given `IMState` + caret_rect, paints one frame and commits the buffer |

Threading note: rendering runs inline on the calloop/wayland thread when the
IC signal lands. swash + tiny-skia for ~9 glyphs at 14 px is sub-millisecond;
no separate render thread is required.

### 3.2 State machine (per `InputContext`)

```
Idle ── ObjectManager.InterfaceAdded ──► Created
Created ── IC.FocusIn ──► Preedit
Preedit ── Updated.has_lookup_table ──► LookupTable
LookupTable ── Updated has no lookup_table ──► Preedit
LookupTable ── CommitStringSignal ──► Done
* ── IC.FocusOut / ObjectManager.Removed ──► Idle
```

`LookupTable` is the only state that paints a popup. `Preedit` paints a thin
preedit strip below the caret if `show_preedit = true`.

### 3.3 Per-flip selection (when multiple ICs are alive)

Single popup rule: only the **latest focused** IC owns the surface. Older ICs
keep their `IMState` so when focus returns they re-emit correctly.

## 4. Protocol interaction

### 4.1 fcitx5 DBus (we only consume)

| Object | Interface | Method / Signal | Use |
|---|---|---|---|
| `org.fcitx.Fcitx5` | `org.fcitx.Fcitx5` | `CreateInputContext` | **not called** |
| `org.fcitx.Fcitx5` | `org.freedesktop.DBus.ObjectManager` | `GetManagedObjects`, `InterfacesAdded`, `InterfacesRemoved` | enumerate ICs |
| `/org/.../InputContext/N` | `org.fcitx.Fcitx5.InputContext1` | `InputContextUpdatedSignal` | paint |
| `/org/.../InputContext/N` | `org.fcitx.Fcitx5.InputContext1` | `CommitStringSignal` | close popup |
| DBus name | `org.fcitx.Fcitx5` | `NameOwnerChanged` | reconnect after crash |

### 4.2 Wayland (we receive + send)

Bindings:

- `wl_compositor` (v4)
- `wl_shm`
- `wl_seat` (first keyboard seat)
- `zwp_text_input_manager_v3` → `zwp_text_input_v3`
- `zwlr_layer_shell_v1` → `zwlr_layer_surface_v1` (when popup needed)

Events consumed:

| Event | Action |
|---|---|
| `enter(surface)` | `state.latest_focus = Some(surface)` |
| `leave(surface)` | if `== latest_focus`: `latest_focus = None`, close popup |
| `cursor_rect(x,y,w,h)` | store surface-local rect; on next paint convert to screen via surface position from `wl_surface` frame + output transform |
| `surrounding_text(...)` | ignored in v1 |
| `commit` | re-send pending on next iteration |
| `configure(serial, w, h)` | ack with same serial, request resize if mismatch |
| `closed` | mark popup as gone; rebuild next time |
| `wl_registry.global_remove` | tear down affected IC |

Per-IC subscription flow:

1. ObjectManager emits `InterfacesAdded` with a new
   `/InputContext/N` object path.
2. Daemon uses `zbus` to construct an `InputContext1ProxyAsync`.
3. Connect `InputContextUpdatedSignal` and `CommitStringSignal`.

### 4.3 Coordinate transform

The wayland protocol does not give a client the screen position of any other
surface, so we approximate. Algorithm on each focus change:

1. `zwp_text_input_v3.enter(surface)` records `focused_surface`.
2. `wl_output.enter(output)` for that surface reads `wl_output.geometry`
   (x, y, w, h) into `focused_output_geometry`.
3. Each `cursor_rect(rx, ry, rw, rh)` is surface-local.
4. Screen-space caret bottom: `(focused_output.x + rx, focused_output.y + ry + rh)`.

Position popup via `zwlr_layer_surface_v1`:

- `set_layer(overlay)` (fall back to `top` if denied)
- `set_anchor(top | left)`
- `set_margin(top = focused_output.y + ry + rh + vertical_offset,
              left = focused_output.x + rx,
              right = 0, bottom = 0)`

Caveat: a tiled surface whose top-left does not equal the focused output's
top-left will be off by that delta. v1 documents it. v2 can mitigate if niri
adds a screen-anchored wayland input-popup protocol.

v1 is approximate; v2 can take a screen rect direct from a future fcitx5 DBus
signal.

## 5. Rendering

### 5.1 Buffer strategy

- One `wl_shm_pool` of 2 MiB (covers 800 x 256 RGBA = 800 KiB plus headroom).
- One main surface of size `(max_width × total_height)` where:
  - `max_width = min(800, floor(focused_output.width / 2))`.
  - `total_height = line_height * rows`; rows = 1 (candidates) + 1 (aux_down)
    + 1 (preedit, conditional).
- Single-row popup holds `1..=9` candidates evenly spaced, with optional
  preedit above and an aux line below.
- Reuse the same `wl_buffer` across frames; on size change, destroy + recreate.

### 5.2 Paint pipeline (per frame)

1. Clear with `text` background.
2. Render optional `aux_up` line at top in smaller font.
3. Render preedit (if any) below caret.
4. Render candidates with equal widths; highlight `cursor_pos` with
   `highlight_bg`.
5. Render `aux_down` line (page indicator) at bottom.
6. Submit `damage` and commit.

Font shaping done by `swash` with `font_fallback_cjk` registered
**before** the primary family. Missing glyphs go to `.notdef` and emit one
warning per glyph across the daemon's lifetime.

## 6. Error handling

| Source | Detected | Action |
|---|---|---|
| fcitx5 gone | `NameOwnerChanged` to `""` | clear all `IMState`; wait for the daemon to come back |
| Single IC death | `InterfacesRemoved` | remove from state map; warn once per loss |
| wayland error | `wl_display` error | log; exit so systemd restarts (RestartSec=2) |
| text-input-v3 malformed | marshal error | drop IC; continue |
| layer-shell ack timeout (200 ms) | local timer | destroy popup; retry on next update |
| popup `closed` event | event | mark gone; rebuild on next `Updated` |
| shm failure | io::Error or `wl_shm` failure | retry once on `/dev/shm` direct fd; if still fails, skip frame |
| font missing glyph | swash shape miss | log once with codepoint; render `.notdef` |

Panic boundaries:

- `config.font_family` resolve failure at start: **panic** with clear message.
- `wl_registry` returns NULL globals during boot: **panic**.
- Any runtime protocol error: **warn and continue**; never panic a real frame.

## 7. Configuration

### 7.1 CLI

```
rime-popup [--config PATH] [--log LEVEL] [--verbose]
```

### 7.2 `~/.config/rime-popup/config.toml`

```toml
[general]
log = "info"

[render]
font_family             = "JetBrains Mono"
font_fallback_cjk       = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"
font_size               = 14
line_padding            = 8
candidate_gap           = 12
highlight_bg            = "#3a3a3a"
text                    = "#e6e6e6"
border                  = "#666666"
border_width            = 1
vertical_offset         = 4

[filter]
ignore_app_ids          = ["emacs"]
show_preedit            = true
show_aux_up             = false
show_aux_down           = true

[wayland]
output_name             = ""
layer                   = "overlay"
anchor                  = "top-left"
```

`ignore_app_ids` is matched against the wayland client's `wl_surface.app_id`
property provided by the tool; if matched, we set `popup_visible = false` for
that surface — keeps emacs-rime from being interrupted by an overlapping
popup.

### 7.3 Signals

- `SIGHUP` → re-read config file.
- `SIGUSR1` → dump IC state to stdout.

## 8. Tests

| Layer | Scope | Tool |
|---|---|---|
| Unit (pure) | `config.rs`, `render/layout.rs`, `state.rs` | `cargo test` |
| Unit (paint) | glyph shaping for ASCII + CJK + emoji fallback | `pretty_assertions` snapshot to PNG via headless skia |
| Integration (DBus) | fake fcitx5 server (use zbus's mock builder) emitting `InputContextUpdatedSignal`; verify IMState | `tests/dbus_sim.rs` |
| Integration (wayland) | in-process `wayland-server` mini compositor; stub `zwp_text_input_manager_v3` + `zwlr_layer_shell_v1`; drive cursor_rect changes; assert marshalled `set_rect` calls | `tests/wayland_mock.rs` |
| System smoke | `cargo build --release`; install under `/usr/local/bin`; enable + start systemd user unit; assert status active | shell script in `tests/smoke.sh` |

Coverage target: **≥ 80%** lines (`cargo-llvm-cov`). CI runs unit + dbus_sim +
wayland_mock + smoke; manual run for paint snapshots.

## 9. Delivery

- Binary: `cargo build --release --locked`
- Location: `/usr/local/bin/rime-popup`
- Unit file path: `~/.config/systemd/user/rime-popup.service`
- License: MIT
- Run on graphical-session.target only
- No Debian packaging in v1 (a single release binary is enough)

`rime-popup.service`:

```ini
[Unit]
Description=Custom fcitx5 candidate popup
PartOf=graphical-session.target
After=graphical-session.target
StartLimitIntervalSec=30
StartLimitBurst=5

[Service]
Type=Simple
ExecStart=/usr/local/bin/rime-popup
Restart=always
RestartSec=2

[Install]
WantedBy=graphical-session.target
```

## 10. Milestones (PRs)

| ID | Goal | Acceptance |
|---|---|---|
| **M1** | wayland client skeleton: shm + layer-shell + a static placeholder popup bound to the cursor via `wl_pointer.motion` (caret hookup not yet wired) | `cargo run` shows a grey box following the pointer on the active output |
| **M2** | DBus side: enumerate ICs, listen to `InputContextUpdated` | logs reflect real ICs from a running fcitx5 |
| **M3** | text-input-v3: caret rect + commit forwarding | manual: `dbus-send` triggers a popup positioned correctly |
| **M4** | swash + tiny-skia: render real candidates with highlight | typing in a GTK app (e.g. `gtk4-widget-factory`) shows a working popup |
| **M5** | hardening: config file, systemd unit, restart, release build | `cargo build --release` ships a binary that survives `kill -9` |

## 11. Constraints (assumed)

- **niri ≥ 25.05** (full `zwlr_layer_shell_v1`)
- **fcitx5 ≥ 5.0** (with `InputContext1` + ObjectManager)
- Single seat only (last focused)
- No DRM/KMS; client only
- Surface-local caret rect → screen transform accepts ±2 px error
- Single-row popup; long lookups scroll horizontally inside the same row

## 12. Open questions carried forward

1. Multi-seat (laptop + dock with own keyboard). v1 ignores second seat silently.
2. Horizontal scrolling when lookup table is longer than 9 candidates and
   we still want a single row. v1 uses a wrapped row; revisit if users complain.
3. We do not flip input method. That remains fcitx5's job.
4. Color theme: light/dark from GTK setting? v1 uses config only.
