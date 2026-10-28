//! Showing a transaction all at once. Clients redraw at a new size in their
//! own time; a transaction that resizes windows waits for them: each one's
//! redraw is held back when it comes, and once all have come (or time's up)
//! they're let through together, with the rest of the transaction, so one
//! frame shows the whole new layout.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use smithay::{
    reexports::{
        calloop::{
            RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        wayland_server::{Client, Resource, protocol::wl_surface::WlSurface},
    },
    utils::Serial,
    wayland::{
        compositor::{Blocker, BlockerState, add_blocker, with_states},
        shell::xdg::XdgToplevelSurfaceData,
    },
};

use crate::{
    Wayluck,
    protocol::wayluck_wm_transaction_v1::WayluckWmTransactionV1,
    state::ClientState,
    windows::{Change, WindowId, Windows},
};

/// How long a transaction waits for slow clients before it's shown anyway.
const PATIENCE: Duration = Duration::from_millis(200);

/// A transaction waiting for clients to redraw.
pub struct Pending {
    /// What's shown once they have: positions, hides, focus, borders.
    changes: Vec<Change>,
    /// Hears applied then.
    tx: Option<WayluckWmTransactionV1>,
    /// Windows whose redraw hasn't come yet, and the configure it answers.
    waiting: HashMap<WindowId, Serial>,
    /// Set when it's shown: the redraws held back go through.
    shown: Arc<AtomicBool>,
    /// Clients with a redraw held back, to tell when it goes through.
    held: Vec<Client>,
    timeout: Option<RegistrationToken>,
    since: Instant,
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
    /// Show changes once the windows in waiting (sent these configures)
    /// have redrawn for them.
    pub fn wait_for(
        &mut self,
        waiting: HashMap<WindowId, Serial>,
        changes: Vec<Change>,
        tx: Option<WayluckWmTransactionV1>,
    ) {
        let timeout = self
            .loop_handle
            .insert_source(Timer::from_duration(PATIENCE), |_, _, state| {
                if let Some(p) = &mut state.pending {
                    p.timeout = None; // this is it; nothing to remove
                    tracing::debug!("transaction: {} client(s) too slow", p.waiting.len());
                }
                state.show_pending();
                TimeoutAction::Drop
            })
            .ok();
        tracing::debug!("transaction: waiting for {} client(s)", waiting.len());
        self.pending = Some(Pending {
            changes,
            tx,
            waiting,
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
        let Some(&serial) = p.waiting.get(&id) else {
            return;
        };
        let acked = with_states(surface, |s| {
            let data = s.data_map.get::<XdgToplevelSurfaceData>()?;
            data.lock().unwrap().configure_serial
        });
        // A commit from before it took the new size isn't the one.
        if !acked.is_some_and(|a| a.is_no_older_than(&serial)) {
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
            if let Some(data) = client.get_data::<ClientState>() {
                data.compositor_state.blocker_cleared(self, &dh);
            }
        }
        self.show(p.changes);
        if let Some(tx) = p.tx {
            tx.applied();
        }
    }
}
