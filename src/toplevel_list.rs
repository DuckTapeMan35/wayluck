//! ext-foreign-toplevel-list: every window, for any client that asks (lswt,
//! screen sharing's window picker), by its id, title and app id. Its id is
//! the one the window manager and wayluck's log know it by: never reused,
//! as the protocol asks. (smithay has this protocol, with identifiers of its
//! own making; this is it with ours.) A window is listed from its first
//! commit, like the window manager hears of it, until it closes.

use std::collections::HashMap;

use smithay::reexports::{
    wayland_protocols::ext::foreign_toplevel_list::v1::server::{
        ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
        ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
    },
    wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource},
};

use crate::{
    Wayluck,
    windows::{self, WindowId},
};

#[derive(Default)]
pub struct ToplevelList {
    /// Clients' lists, until they stop.
    lists: Vec<ExtForeignToplevelListV1>,
    /// Each listed window's handles, one for each list.
    handles: HashMap<WindowId, Vec<ExtForeignToplevelHandleV1>>,
}

impl Wayluck {
    pub fn toplevel_list_global(dh: &DisplayHandle) {
        dh.create_global::<Wayluck, ExtForeignToplevelListV1, _>(1, ());
    }

    /// List a window (once): every list gets a handle for it.
    pub fn list_window(&mut self, id: WindowId) {
        if self.toplevel_list.handles.contains_key(&id) {
            return;
        }
        let lists = self.toplevel_list.lists.clone();
        let handles = (lists.iter())
            .filter_map(|list| self.new_handle(list, id))
            .collect();
        self.toplevel_list.handles.insert(id, handles);
    }

    /// A handle for window id on list, with all there is to know about it.
    fn new_handle(
        &self,
        list: &ExtForeignToplevelListV1,
        id: WindowId,
    ) -> Option<ExtForeignToplevelHandleV1> {
        let client = list.client()?;
        let handle = client
            .create_resource::<ExtForeignToplevelHandleV1, WindowId, Wayluck>(
                &self.display_handle,
                list.version(),
                id,
            )
            .ok()?;
        list.toplevel(&handle);
        handle.identifier(u32::from(id).to_string());
        self.send_names(&handle, id);
        handle.done();
        Some(handle)
    }

    /// A listed window's title or app id changed: every list hears.
    pub fn list_names_changed(&self, id: WindowId) {
        for handle in self.toplevel_list.handles.get(&id).into_iter().flatten() {
            self.send_names(handle, id);
            handle.done();
        }
    }

    fn send_names(&self, handle: &ExtForeignToplevelHandleV1, id: WindowId) {
        let Some(toplevel) = self.windows.get(id).and_then(|w| w.toplevel()) else {
            return;
        };
        let (title, app_id) = windows::names(toplevel);
        handle.title(title.unwrap_or_default());
        handle.app_id(app_id.unwrap_or_default());
    }

    /// A window closed: off every list.
    pub fn unlist_window(&mut self, id: WindowId) {
        for handle in self.toplevel_list.handles.remove(&id).into_iter().flatten() {
            handle.closed();
        }
    }
}

impl GlobalDispatch<ExtForeignToplevelListV1, ()> for Wayluck {
    /// A new list: every window listed so far is on it.
    fn bind(
        state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtForeignToplevelListV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let list = data_init.init(resource, ());
        let ids: Vec<WindowId> = state.toplevel_list.handles.keys().copied().collect();
        for id in ids {
            if let Some(handle) = state.new_handle(&list, id) {
                state
                    .toplevel_list
                    .handles
                    .entry(id)
                    .or_default()
                    .push(handle);
            }
        }
        state.toplevel_list.lists.push(list);
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        list: &ExtForeignToplevelListV1,
        request: ext_foreign_toplevel_list_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // Stop: no more windows on it (its handles stay, closing as their
        // windows close).
        if let ext_foreign_toplevel_list_v1::Request::Stop = request {
            state.toplevel_list.lists.retain(|l| l != list);
            list.finished();
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        list: &ExtForeignToplevelListV1,
        _data: &(),
    ) {
        state.toplevel_list.lists.retain(|l| l != list);
    }
}

impl Dispatch<ExtForeignToplevelHandleV1, WindowId> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _handle: &ExtForeignToplevelHandleV1,
        _request: ext_foreign_toplevel_handle_v1::Request,
        _id: &WindowId,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // Only destroy.
    }

    fn destroyed(
        state: &mut Self,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        handle: &ExtForeignToplevelHandleV1,
        id: &WindowId,
    ) {
        if let Some(handles) = state.toplevel_list.handles.get_mut(id) {
            handles.retain(|h| h != handle);
        }
    }
}
