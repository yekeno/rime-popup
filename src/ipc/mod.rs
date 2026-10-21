//! Bridge from the tokio fcitx5 driver into the calloop wayland thread.
//!
//! One channel carries every message that needs the wayland thread's attention:
//! a focus change, a new candidate list, a request to commit, or an input
//! context disappearing. The tokio task owns a [`Sender`]; the calloop thread
//! owns the [`Channel`] as an event source and dispatches each message.
//!
//! Messages carry an [`IcId`] rather than a Dbus path so they compare and hash
//! the same way `crate::state` keys its per-IC map.

use calloop::channel;
use calloop::channel::Channel;

use crate::fcitx5::events::LookupTable;
use crate::state::IcId;

/// Re-exported so the runtime can name the event type when registering the
/// channel with calloop, without reaching into calloop's module layout.
pub use calloop::channel::{Event, Sender};

#[derive(Debug, Clone)]
pub enum DaemonToWayland {
    /// An input context took focus.
    FocusIn(IcId),
    /// An input context lost focus; any popup for it must be dismissed.
    FocusOut(IcId),
    /// fcitx5 published a new candidate list.
    LookupTable { ic: IcId, table: LookupTable },
    /// A commit finished; the popup may be retired.
    Commit(IcId),
    /// The input context object itself was destroyed.
    Removed(IcId),
}

impl DaemonToWayland {
    /// The input context this message concerns.
    pub fn ic(&self) -> &IcId {
        match self {
            Self::FocusIn(ic)
            | Self::FocusOut(ic)
            | Self::LookupTable { ic, .. }
            | Self::Commit(ic)
            | Self::Removed(ic) => ic,
        }
    }
}

pub fn make_pair() -> (Sender<DaemonToWayland>, Channel<DaemonToWayland>) {
    channel::channel()
}

/// Drain everything currently queued, without blocking.
///
/// `Channel::recv` blocks until the next message, which would stall the
/// wayland thread; `try_recv` is the non-blocking form and returns
/// `TryRecvError::Disconnected` once the tokio side has dropped its sender.
pub fn drain(chan: &Channel<DaemonToWayland>) -> Vec<DaemonToWayland> {
    let mut out = Vec::new();
    while let Ok(msg) = chan.try_recv() {
        out.push(msg);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn ic() -> IcId {
        Arc::from("/InputContext/0")
    }

    #[test]
    fn send_recv_roundtrip() {
        let (tx, rx) = make_pair();
        tx.send(DaemonToWayland::FocusIn(ic())).expect("send");
        tx.send(DaemonToWayland::LookupTable {
            ic: ic(),
            table: LookupTable::default(),
        })
        .expect("send");

        let got = drain(&rx);
        assert_eq!(got.len(), 2);
        assert!(matches!(got[0], DaemonToWayland::FocusIn(_)));
        assert!(matches!(got[1], DaemonToWayland::LookupTable { .. }));
    }

    #[test]
    fn drain_preserves_order() {
        let (tx, rx) = make_pair();
        for i in 0..5 {
            tx.send(DaemonToWayland::Commit(Arc::from(
                format!("/InputContext/{i}").as_str(),
            )))
            .expect("send");
        }
        let got = drain(&rx);
        assert_eq!(
            got.iter().map(|m| m.ic().to_string()).collect::<Vec<_>>(),
            (0..5)
                .map(|i| format!("/InputContext/{i}"))
                .collect::<Vec<_>>(),
            "candidate updates must not be reordered"
        );
    }

    #[test]
    fn drain_on_empty_is_empty() {
        let (_tx, rx) = make_pair();
        assert!(drain(&rx).is_empty());
    }

    #[test]
    fn drain_after_sender_dropped_terminates() {
        let (tx, rx) = make_pair();
        tx.send(DaemonToWayland::FocusOut(ic())).expect("send");
        drop(tx);
        let got = drain(&rx);
        assert_eq!(got.len(), 1, "queued messages survive the sender");
    }

    #[test]
    fn every_variant_reports_its_ic() {
        let id = ic();
        let msgs = [
            DaemonToWayland::FocusIn(id.clone()),
            DaemonToWayland::FocusOut(id.clone()),
            DaemonToWayland::LookupTable {
                ic: id.clone(),
                table: LookupTable::default(),
            },
            DaemonToWayland::Commit(id.clone()),
            DaemonToWayland::Removed(id.clone()),
        ];
        for m in &msgs {
            assert_eq!(m.ic(), &id);
        }
    }

    #[test]
    fn event_type_is_reachable_for_event_loop_registration() {
        // The runtime registers `Channel<DaemonToWayland>` with calloop and
        // matches on `Event<DaemonToWayland>`; naming it here keeps the
        // re-export honest.
        let e: Event<DaemonToWayland> = Event::Closed;
        assert!(matches!(e, Event::Closed));
    }
}
