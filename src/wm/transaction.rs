//! Transactions: the window manager's changes, collected until it commits
//! them, then applied together (or not at all).

use super::{
    id_of,
    protocol::{
        wayluck_wm_manager_v1::WayluckWmManagerV1,
        wayluck_wm_transaction_v1::{self, WayluckWmTransactionV1},
    },
    scene_of,
};
use crate::{
    Wayluck,
    render::{argb, rounded::Outline, shadow::Spec},
    windows::Change,
};
use smithay::{
    output::Output,
    reexports::wayland_server::{Client, DataInit, Dispatch, DisplayHandle, Resource},
    utils::Rectangle,
};
use std::sync::Mutex;

/// A transaction: its changes, collected until commit.
pub struct Transaction {
    /// Only the bound manager's transactions are applied.
    manager: WayluckWmManagerV1,
    pending: Mutex<Pending>,
}

#[derive(Default)]
struct Pending {
    changes: Vec<Change>,
    committed: bool,
}

impl Transaction {
    pub(super) fn new(manager: &WayluckWmManagerV1) -> Self {
        Self {
            manager: manager.clone(),
            pending: Mutex::default(),
        }
    }
}

impl Dispatch<WayluckWmTransactionV1, Transaction> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        transaction: &WayluckWmTransactionV1,
        request: wayluck_wm_transaction_v1::Request,
        data: &Transaction,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use wayluck_wm_transaction_v1::Request;
        // A change to collect, or None for commit.
        let change = match request {
            Request::Destroy => return,
            Request::Place {
                scene,
                window,
                x,
                y,
                width,
                height,
            } => {
                let rect = Rectangle::new((x, y).into(), (width, height).into());
                Some(Change::Place(scene_of(&scene), id_of(&window), rect))
            }
            Request::SceneSize {
                scene,
                width,
                height,
            } => Some(Change::SceneSize(scene_of(&scene), (width, height).into())),
            Request::Show { output, scene } => {
                let Some(output) = output.data::<Output>().cloned() else {
                    return;
                };
                Some(Change::Show(output, scene.as_ref().map(scene_of)))
            }
            Request::Border {
                window,
                width,
                top,
                right,
                bottom,
                left,
                radius,
            } => Some(Change::Border(
                id_of(&window),
                width,
                [top, right, bottom, left],
                radius,
            )),
            Request::Shadow {
                window,
                blur,
                spread,
                x,
                y,
                color,
            } => Some(Change::Shadow(
                id_of(&window),
                Spec {
                    blur,
                    spread,
                    offset: (x, y).into(),
                    color,
                },
            )),
            Request::Tiled { window, edges } => Some(Change::Tiled(id_of(&window), edges)),
            Request::Outlines { scene } => Some(Change::Outlines(scene_of(&scene))),
            Request::Outline {
                scene,
                x,
                y,
                width,
                height,
                clip_x,
                clip_y,
                clip_width,
                clip_height,
                line_width,
                color,
                radius,
            } => {
                let rect = Rectangle::new((x, y).into(), (width, height).into());
                let clip =
                    Rectangle::new((clip_x, clip_y).into(), (clip_width, clip_height).into());
                let color = argb(color).components();
                let outline = Outline {
                    rect,
                    clip,
                    width: line_width,
                    color,
                    radius,
                };
                Some(Change::Outline(scene_of(&scene), outline))
            }
            Request::Fullscreen { window, fullscreen } => {
                Some(Change::Fullscreen(id_of(&window), fullscreen != 0))
            }
            Request::Hide { window } => Some(Change::Hide(id_of(&window))),
            Request::Instant { window } => Some(Change::Instant(window.as_ref().map(id_of))),
            Request::StartFrom {
                window,
                x,
                y,
                width,
                height,
            } => {
                let from = Rectangle::new((x, y).into(), (width, height).into());
                Some(Change::StartFrom(id_of(&window), from))
            }
            Request::Commit => None,
            Request::Focus { window } => Some(Change::Focus(window.as_ref().map(id_of))),
        };
        let mut pending = data.pending.lock().unwrap();
        if pending.committed {
            transaction.post_error(
                wayluck_wm_transaction_v1::Error::AlreadyCommitted,
                "a request after commit",
            );
            return;
        }
        if let Some(change) = change {
            pending.changes.push(change);
            return;
        }
        pending.committed = true;
        let changes = std::mem::take(&mut pending.changes);
        drop(pending);
        // applied comes from apply, once it's on screen.
        let result = if state.wm.manager.as_ref() == Some(&data.manager) {
            state.apply(changes, Some(transaction.clone()))
        } else {
            Err("not from the window manager")
        };
        if let Err(why) = result {
            transaction.failed(why.into());
        }
    }
}
