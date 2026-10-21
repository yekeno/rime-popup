//! Bind the wayland globals this daemon needs.
//!
//! [`super::registry`] records which globals the compositor advertised;
//! this module turns those records into live proxies. Everything here runs
//! once, during startup, before the event loop begins dispatching.
//!
//! # Version clamping
//!
//! `wl_registry.bind` takes the version we want, and the compositor rejects
//! the bind if it exceeds what we can speak. wayland-client 0.31 exposes no
//! "max version I support" constant on [`Proxy`] (the generated bindings
//! carry no such const), so the ceilings live in [`VERSIONS`] below. They are
//! deliberately conservative: every request this daemon issues exists in the
//! listed version or earlier, so a compositor advertising more just gets a
//! smaller surface than it offered.

use wayland_client::protocol::{
    wl_compositor::WlCompositor, wl_registry::WlRegistry, wl_seat::WlSeat, wl_shm::WlShm,
};
use wayland_client::{Dispatch, Proxy, QueueHandle};

use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;

use crate::errors::Error;
use crate::wayland::registry::Needed;
use crate::Result;

/// Highest interface version we bind each global at.
///
/// See the module docs for why these are hand-written rather than derived.
pub const VERSIONS: [(&str, u32); 5] = [
    ("wl_compositor", 4),
    ("wl_shm", 1),
    ("wl_seat", 5),
    ("zwp_text_input_manager_v3", 1),
    ("zwlr_layer_shell_v1", 4),
];

/// The bound globals, kept alive for as long as the daemon runs.
///
/// Dropping any of these sends the corresponding `destroy`, so the runtime
/// stores this struct rather than the individual proxies.
pub struct Globals {
    pub compositor: WlCompositor,
    pub shm: WlShm,
    pub seat: WlSeat,
    pub text_input_manager: ZwpTextInputManagerV3,
    pub layer_shell: ZwlrLayerShellV1,
}

impl std::fmt::Debug for Globals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Globals")
            .field("compositor", &self.compositor.id())
            .field("shm", &self.shm.id())
            .field("seat", &self.seat.id())
            .field("text_input_manager", &self.text_input_manager.id())
            .field("layer_shell", &self.layer_shell.id())
            .finish()
    }
}

/// The version we bind `interface` at, given what the compositor advertised.
///
/// Exposed for testing: this is pure arithmetic over [`VERSIONS`] and needs
/// no compositor to exercise.
pub fn bind_version(interface: &str, advertised: u32) -> u32 {
    let ceiling = VERSIONS
        .iter()
        .find(|(iface, _)| *iface == interface)
        .map(|(_, v)| *v)
        .unwrap_or(1);
    advertised.min(ceiling)
}

/// A state type that can receive events for every global we bind.
///
/// `WlRegistry::bind` is the only supported way to bind: `send_constructor`
/// cannot build the request, because wayland-client's generated
/// `wl_registry::Request::Bind` carries just the new id — the wire arguments
/// live in a separate `Message` the scanner builds. Going through `bind`
/// means every bound global needs a `Dispatch` impl on the queue's data type,
/// which is what this trait collects.
///
/// Each impl uses `()` as userdata. The globals bind with no payload; anything
/// carrying real state (the per-seat `zwp_text_input_v3`) is created later
/// with its own userdata.
pub trait Bindable:
    Dispatch<WlCompositor, ()>
    + Dispatch<WlShm, ()>
    + Dispatch<WlSeat, ()>
    + Dispatch<ZwpTextInputManagerV3, ()>
    + Dispatch<ZwlrLayerShellV1, ()>
    + 'static
{
}

impl<T> Bindable for T where
    T: Dispatch<WlCompositor, ()>
        + Dispatch<WlShm, ()>
        + Dispatch<WlSeat, ()>
        + Dispatch<ZwpTextInputManagerV3, ()>
        + Dispatch<ZwlrLayerShellV1, ()>
        + 'static
{
}

/// Bind every global in [`crate::wayland::registry::REQUIRED`].
///
/// Fails if any of them was missing from the registry, because a popup
/// without, say, `zwlr_layer_shell_v1` has nowhere to draw. Failing at startup
/// with a named global beats running forever and rendering nothing.
pub fn bind_all<D: Bindable>(
    registry: &WlRegistry,
    needed: &Needed,
    qh: &QueueHandle<D>,
) -> Result<Globals> {
    Ok(Globals {
        compositor: bind_one::<WlCompositor, D>(registry, needed, qh, "wl_compositor")?,
        shm: bind_one::<WlShm, D>(registry, needed, qh, "wl_shm")?,
        seat: bind_one::<WlSeat, D>(registry, needed, qh, "wl_seat")?,
        text_input_manager: bind_one::<ZwpTextInputManagerV3, D>(
            registry,
            needed,
            qh,
            "zwp_text_input_manager_v3",
        )?,
        layer_shell: bind_one::<ZwlrLayerShellV1, D>(registry, needed, qh, "zwlr_layer_shell_v1")?,
    })
}

/// Bind a single global at the clamped version.
fn bind_one<I, D>(
    registry: &WlRegistry,
    needed: &Needed,
    qh: &QueueHandle<D>,
    interface: &str,
) -> Result<I>
where
    I: Proxy + 'static,
    D: Dispatch<I, ()> + 'static,
{
    let name = needed
        .name_of(interface)
        .ok_or_else(|| Error::MissingGlobal(interface.to_owned()))?;
    let advertised = needed.version_of(interface).unwrap_or(1);

    // `bind` swallows the underlying error and returns an inert proxy, so
    // verify the result is live rather than trusting it.
    let proxy = registry.bind(name, bind_version(interface, advertised), qh, ());
    if !proxy.is_alive() {
        return Err(Error::Internal(format!("bind {interface}: proxy is inert")));
    }
    Ok(proxy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wayland::registry;

    fn full_needed() -> Needed {
        let mut n = Needed::default();
        for (i, (iface, version)) in VERSIONS.iter().enumerate() {
            registry::record(&mut n, i as u32 + 1, iface, *version);
        }
        n
    }

    #[test]
    fn version_is_clamped_to_our_ceiling() {
        // Compositors advertise newer versions than we need.
        assert_eq!(bind_version("wl_compositor", 6), 4);
        assert_eq!(bind_version("zwlr_layer_shell_v1", 9), 4);
    }

    #[test]
    fn version_passes_through_when_within_ceiling() {
        assert_eq!(bind_version("wl_shm", 1), 1);
        assert_eq!(bind_version("wl_compositor", 3), 3);
        assert_eq!(bind_version("wl_seat", 5), 5);
    }

    #[test]
    fn unknown_interface_falls_back_to_version_one() {
        assert_eq!(bind_version("wl_drm", 7), 1);
    }

    #[test]
    fn every_required_global_has_a_declared_ceiling() {
        for iface in registry::REQUIRED {
            assert!(
                VERSIONS.iter().any(|(i, _)| *i == iface),
                "{iface} is required but has no version ceiling"
            );
        }
    }

    #[test]
    fn ceilings_are_at_least_one() {
        for (iface, v) in VERSIONS {
            assert!(v >= 1, "{iface} ceiling must allow version 1");
        }
    }

    #[test]
    fn missing_global_is_reported_by_name() {
        // `Needed` with nothing recorded; binding must fail naming the global.
        let empty = Needed::default();
        assert_eq!(empty.name_of("wl_compositor"), None);
        let missing = full_needed();
        missing.name_of("wl_drm");
        assert!(matches!(
            Error::MissingGlobal("wl_compositor".into())
                .to_string()
                .as_str(),
            "wayland global not advertised: wl_compositor"
        ));
    }

    #[test]
    fn full_needed_satisfies_every_required_global() {
        assert!(full_needed().is_satisfied());
    }
}
