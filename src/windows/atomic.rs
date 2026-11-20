//! Showing a transaction all at once. Clients redraw at a new size in their
//! own time; a transaction that resizes windows waits for them: each one's
//! redraw is held back when it comes, and once all have come (or time's up)
//! they're let through together, with the rest of the transaction, so one
//! frame shows the whole new layout.

use crate::{
    Wayluck,
    surfaces::compositor_state_of,
    windows::{Change, WindowId, Windows, names},
    wm::protocol::wayluck_wm_transaction_v1::WayluckWmTransactionV1,
};
use smithay::{
    reexports::{
        calloop::{
            RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        wayland_server::{Client, Resource, protocol::wl_surface::WlSurface},
    },
    utils::{Logical, Rectangle, Serial, Size},
    wayland::{
        compositor::{Blocker, BlockerState, add_blocker, with_states},
        shell::xdg::XdgToplevelSurfaceData,
    },
    xwayland::X11Surface,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// How long a transaction waits for slow clients before it's shown anyway.
pub(super) const PATIENCE: Duration = Duration::from_millis(200);

/// How long one waits for a new window's first frame: the room made for it
/// isn't shown before, unless its client never draws.
const FIRST_FRAME: Duration = Duration::from_secs(2);

/// How long Xwayland's commit is waited for once an X window's client says
/// it has drawn: it comes at once, unless nothing changed on screen.
const X_COMMIT: Duration = Duration::from_millis(20);

/// A transaction waiting for clients to redraw.
pub struct Pending {
    /// What's shown once they have: positions, hides, focus, borders.
    changes: Vec<Change>,
    /// Hears applied then.
    tx: Option<WayluckWmTransactionV1>,
    /// Windows whose redraw hasn't come yet, and which redraw it is.
    waiting: HashMap<WindowId, Redraw>,
    /// X windows whose clients said they've drawn: not waited for, but
    /// what Xwayland commits of them waits with the rest.
    drawn_x: HashSet<WindowId>,
    /// Set when it's shown: the redraws held back go through.
    shown: Arc<AtomicBool>,
    /// Clients with a redraw held back, to tell when it goes through.
    held: Vec<Client>,
    timeout: Option<RegistrationToken>,
    since: Instant,
}

/// The redraw a transaction waits for from a window.
#[derive(Clone, Copy, PartialEq)]
pub enum Redraw {
    /// A Wayland window's: its first commit after acking this configure.
    Acked(Serial),
    /// An X window's, at this size: its first commit after its client said
    /// it has drawn at it (_NET_WM_SYNC_REQUEST). Till then, its commits are
    /// held back in Xwayland.
    Synced(Size<i32, Logical>),
}

/// Holds a commit back until the transaction it belongs to is shown.
struct Hold(Arc<AtomicBool>);

impl Blocker for Hold {
    fn state(&self) -> BlockerState {
        if self.0.load(Ordering::Acquire) {
            BlockerState::Released
        } else {
            BlockerState::Pending
        }
    }
}

impl Wayluck {
    /// Show changes once the windows in waiting (sent these configures) have redrawn for them.
    pub fn wait_for(
        &mut self,
        waiting: HashMap<WindowId, Redraw>,
        changes: Vec<Change>,
        tx: Option<WayluckWmTransactionV1>,
    ) {
        let timeout = self
            .loop_handle
            .insert_source(Timer::from_duration(PATIENCE), |_, _, state| {
                let Some(p) = &mut state.pending else {
                    return TimeoutAction::Drop;
                };
                // Waiting for a new window's first frame: on, looking again
                // as often (one that's only redrawing may be what's left).
                let first_frame = (p.waiting.keys())
                    .filter_map(|id| state.windows.get(*id))
                    .any(|w| w.geometry().size.is_empty());
                let left = FIRST_FRAME.saturating_sub(p.since.elapsed());
                if first_frame && !left.is_zero() {
                    return TimeoutAction::ToDuration(left.min(PATIENCE));
                }
                p.timeout = None; // this is it; nothing to remove
                // Which, by name, and whether they've drawn at all yet.
                let slow: Vec<_> = (p.waiting.keys())
                    .filter_map(|id| state.windows.get(*id))
                    .map(|w| (names(w), w.geometry().size.is_empty()))
                    .collect();
                tracing::debug!("transaction: too slow (names, never drawn): {slow:?}");
                state.show_pending();
                TimeoutAction::Drop
            })
            .ok();
        tracing::debug!("transaction: waiting for {} client(s)", waiting.len());
        self.pending = Some(Pending {
            changes,
            tx,
            waiting,
            drawn_x: HashSet::new(),
            shown: Arc::new(AtomicBool::new(false)),
            held: Vec::new(),
            timeout,
            since: Instant::now(),
        });
    }

    /// Before a commit of a window's surface (a pre-commit hook): if it's the
    /// redraw a transaction waits for, it's held back until the transaction
    /// is shown.
    pub fn hold_back(&mut self, surface: &WlSurface) {
        let Some(p) = &mut self.pending else {
            return;
        };
        let Some(id) = Windows::id_of(surface) else {
            return;
        };
        if p.drawn_x.contains(&id) {
            add_blocker(surface, Hold(p.shown.clone()));
            p.held.extend(surface.client());
            return;
        }
        let Some(&redraw) = p.waiting.get(&id) else {
            return;
        };
        // A commit from before it took the new size isn't the one.
        let drawn = match redraw {
            Redraw::Acked(serial) => {
                let acked = with_states(surface, |s| {
                    let data = s.data_map.get::<XdgToplevelSurfaceData>()?;
                    data.lock().unwrap().last_acked.as_ref().map(|c| c.serial)
                });
                acked.is_some_and(|a| a.is_no_older_than(&serial))
            }
            Redraw::Synced(size) => (self.windows.get(id))
                .and_then(|w| w.x11_surface())
                .is_some_and(|x| {
                    x.pending_configure().is_none() && x.last_configure().size == size
                }),
        };
        if !drawn {
            return;
        }
        add_blocker(surface, Hold(p.shown.clone()));
        if let Some(client) = surface.client() {
            p.held.push(client);
        }
        p.waiting.remove(&id);
        if p.waiting.is_empty() {
            // The last one: show it all, once this commit is done.
            self.loop_handle.insert_idle(|state| state.show_pending());
        }
    }

    /// An X window's client said it has drawn at the size it was told.
    /// Xwayland commits what it drew at once, and that's the redraw waited
    /// for; but if nothing changed on screen no commit comes, so it's only
    /// waited for a moment.
    pub fn x_synced(&mut self, window: &X11Surface) {
        let Some(id) = self.windows.find_x11(window) else {
            return;
        };
        let Some(p) = &mut self.pending else {
            return;
        };
        let Some(Redraw::Synced(size)) = p.waiting.get(&id).copied() else {
            return;
        };
        // An earlier size: the one waited for may be held back in smithay
        // till Xwayland commits, which it may not (if nothing changed).
        // Asked for now.
        if window.last_configure().size != size {
            let rect = Rectangle::new(window.last_configure().loc, size);
            if let Err(e) = window.configure_with_sync(rect, Some(PATIENCE)) {
                tracing::debug!("X: can't resize window {}: {e}", window.window_id());
            }
            return;
        }
        let late = move |_, _: &mut (), state: &mut Wayluck| {
            state.x_drew_nothing(id, size);
            TimeoutAction::Drop
        };
        let _ = self
            .loop_handle
            .insert_source(Timer::from_duration(X_COMMIT), late);
    }

    /// No commit came from X window id after its client said it drew at
    /// size: nothing changed on screen. It isn't waited for; what Xwayland
    /// commits of it later waits with the rest.
    fn x_drew_nothing(&mut self, id: WindowId, size: Size<i32, Logical>) {
        let Some(p) = &mut self.pending else {
            return;
        };
        if p.waiting.get(&id) != Some(&Redraw::Synced(size)) {
            return; // its commit came
        }
        p.waiting.remove(&id);
        p.drawn_x.insert(id);
        if p.waiting.is_empty() {
            self.loop_handle.insert_idle(|state| state.show_pending());
        }
    }

    /// A window closed: a transaction waiting for it waits no more.
    pub fn stop_waiting_for(&mut self, id: WindowId) {
        let Some(p) = &mut self.pending else {
            return;
        };
        if p.waiting.remove(&id).is_some() && p.waiting.is_empty() {
            self.loop_handle.insert_idle(|state| state.show_pending());
        }
    }

    /// Show the waiting transaction now: the redraws held back go through,
    /// and the rest of it is applied with them.
    pub fn show_pending(&mut self) {
        let Some(p) = self.pending.take() else {
            return;
        };
        if let Some(t) = p.timeout {
            self.loop_handle.remove(t);
        }
        tracing::debug!("transaction: shown after {:?}", p.since.elapsed());
        p.shown.store(true, Ordering::Release);
        let dh = self.display_handle.clone();
        for client in p.held {
            if let Some(state) = compositor_state_of(&client) {
                state.blocker_cleared(self, &dh);
            }
        }
        self.show(p.changes);
        if let Some(tx) = p.tx {
            tx.applied();
        }
    }
}
