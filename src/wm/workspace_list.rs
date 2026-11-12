//! ext-workspace-v1: the window manager's workspaces, for Wayland bars and
//! pagers, as X's see them as desktops. Workspaces on an output are in a
//! group for it; those on none in particular, in one group for all
//! monitors. A bar asking to show one asks the window manager, which
//! decides; nothing else is offered (creating, removing, moving them).

use super::workspaces::{State, WorkspaceId};
use crate::Wayluck;
use smithay::{
    output::Output,
    reexports::{
        wayland_protocols::ext::workspace::v1::server::{
            ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1, GroupCapabilities},
            ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1, WorkspaceCapabilities},
            ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            backend::ClientId, protocol::wl_output::WlOutput,
        },
    },
};
use std::sync::Mutex;

/// Every client's view of the workspaces.
#[derive(Default)]
pub struct WorkspaceList {
    bound: Vec<Bound>,
}

/// One client's view: what it's been told.
struct Bound {
    manager: ExtWorkspaceManagerV1,
    groups: Vec<Group>,
    workspaces: Vec<Told>,
}

/// A group: the workspaces on an output, or on none in particular (None).
struct Group {
    output: Option<Output>,
    handle: ExtWorkspaceGroupHandleV1,
    /// The client's wl_outputs it's said to be on.
    entered: Vec<WlOutput>,
}

/// A workspace, and what the client was last told of it.
struct Told {
    id: WorkspaceId,
    handle: ExtWorkspaceHandleV1,
    name: String,
    state: State,
    number: u32,
    /// The group it's in (None: none yet).
    group: Option<Option<Output>>,
}

/// What a client's manager carries: the workspaces asked to be shown,
/// until it commits.
#[derive(Default)]
pub struct Asked(Mutex<Vec<WorkspaceId>>);

/// What a client's workspace handle carries.
pub struct Handle {
    id: WorkspaceId,
    manager: ExtWorkspaceManagerV1,
}

impl Wayluck {
    pub fn workspace_list_global(dh: &DisplayHandle) {
        dh.create_global::<Wayluck, ExtWorkspaceManagerV1, _>(1, ());
    }

    /// What's said of workspaces (or the outputs they're on) changed: every
    /// client hears what's new.
    pub fn workspace_list_changed(&mut self) {
        let mut bound = std::mem::take(&mut self.workspace_list.bound);
        for b in &mut bound {
            self.tell(b);
        }
        self.workspace_list.bound = bound;
    }

    fn tell(&self, b: &mut Bound) {
        let Some(client) = b.manager.client() else {
            return;
        };
        let mut told = false;
        // A group for each output workspaces are on, in their order.
        let mut keys: Vec<Option<Output>> = Vec::new();
        for workspace in self.workspaces.iter() {
            if !keys.contains(&workspace.output) {
                keys.push(workspace.output.clone());
            }
        }
        for key in &keys {
            if b.groups.iter().any(|g| &g.output == key) {
                continue;
            }
            let Some(handle) = self.new_resource::<ExtWorkspaceGroupHandleV1, _>(&client, b, ())
            else {
                continue;
            };
            b.manager.workspace_group(&handle);
            handle.capabilities(GroupCapabilities::empty());
            let output = key.clone();
            let entered = Vec::new();
            b.groups.push(Group {
                output,
                handle,
                entered,
            });
            told = true;
        }
        // Each group's outputs, as the client's wl_outputs for them.
        for group in &mut b.groups {
            let outputs: Vec<Output> = match &group.output {
                Some(output) => vec![output.clone()],
                None => (self.space.outputs())
                    .filter(|o| !self.is_virtual(o))
                    .cloned()
                    .collect(),
            };
            let wanted: Vec<WlOutput> = (outputs.iter())
                .flat_map(|o| o.client_outputs(&client))
                .collect();
            for output in wanted.iter().filter(|o| !group.entered.contains(o)) {
                group.handle.output_enter(output);
                told = true;
            }
            for output in group.entered.iter().filter(|o| !wanted.contains(o)) {
                if output.is_alive() {
                    group.handle.output_leave(output);
                    told = true;
                }
            }
            group.entered = wanted;
        }
        // The workspaces, numbered in order.
        for (number, workspace) in self.workspaces.iter().enumerate() {
            let number = number as u32;
            let at = b.workspaces.iter().position(|t| t.id == workspace.id);
            let at = match at {
                Some(at) => at,
                None => {
                    let data = Handle {
                        id: workspace.id,
                        manager: b.manager.clone(),
                    };
                    let Some(handle) =
                        self.new_resource::<ExtWorkspaceHandleV1, _>(&client, b, data)
                    else {
                        continue;
                    };
                    b.manager.workspace(&handle);
                    handle.capabilities(WorkspaceCapabilities::Activate);
                    handle.name(workspace.name.clone());
                    handle.coordinates(number.to_ne_bytes().to_vec());
                    handle.state(bits(workspace.state));
                    b.workspaces.push(Told {
                        id: workspace.id,
                        handle,
                        name: workspace.name.clone(),
                        state: workspace.state,
                        number,
                        group: None,
                    });
                    told = true;
                    b.workspaces.len() - 1
                }
            };
            let t = &mut b.workspaces[at];
            if t.name != workspace.name {
                t.handle.name(workspace.name.clone());
                t.name = workspace.name.clone();
                told = true;
            }
            if t.number != number {
                t.handle.coordinates(number.to_ne_bytes().to_vec());
                t.number = number;
                told = true;
            }
            if t.state != workspace.state {
                t.handle.state(bits(workspace.state));
                t.state = workspace.state;
                told = true;
            }
            if t.group.as_ref() != Some(&workspace.output) {
                if let Some(old) = &t.group
                    && let Some(group) = b.groups.iter().find(|g| &g.output == old)
                {
                    group.handle.workspace_leave(&t.handle);
                }
                if let Some(group) = b.groups.iter().find(|g| g.output == workspace.output) {
                    group.handle.workspace_enter(&t.handle);
                }
                t.group = Some(workspace.output.clone());
                told = true;
            }
        }
        // Workspaces gone: out of their group first, as groups gone are.
        b.workspaces.retain(|t| {
            if self.workspaces.exists(t.id) {
                return true;
            }
            if let Some(old) = &t.group
                && let Some(group) = b.groups.iter().find(|g| &g.output == old)
            {
                group.handle.workspace_leave(&t.handle);
            }
            t.handle.removed();
            told = true;
            false
        });
        b.groups.retain(|g| {
            if keys.contains(&g.output) {
                return true;
            }
            g.handle.removed();
            told = true;
            false
        });
        if told {
            b.manager.done();
        }
    }

    fn new_resource<I, D>(&self, client: &Client, b: &Bound, data: D) -> Option<I>
    where
        I: Resource + 'static,
        D: Send + Sync + 'static,
        Wayluck: Dispatch<I, D>,
    {
        let version = b.manager.version();
        (client.create_resource::<I, D, Wayluck>(&self.display_handle, version, data)).ok()
    }
}

/// A workspace's state, as the protocol has it.
fn bits(state: State) -> ext_workspace_handle_v1::State {
    let mut bits = ext_workspace_handle_v1::State::empty();
    bits.set(ext_workspace_handle_v1::State::Active, state.active);
    bits.set(ext_workspace_handle_v1::State::Urgent, state.urgent);
    bits.set(ext_workspace_handle_v1::State::Hidden, state.hidden);
    bits
}

impl GlobalDispatch<ExtWorkspaceManagerV1, ()> for Wayluck {
    /// A new client's view: told everything there is at once.
    fn bind(
        state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtWorkspaceManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, Asked::default());
        let mut bound = Bound {
            manager,
            groups: Vec::new(),
            workspaces: Vec::new(),
        };
        state.tell(&mut bound);
        state.workspace_list.bound.push(bound);
    }
}

impl Dispatch<ExtWorkspaceManagerV1, Asked> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &ExtWorkspaceManagerV1,
        request: ext_workspace_manager_v1::Request,
        asked: &Asked,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            // The window manager hears what's asked, and decides.
            ext_workspace_manager_v1::Request::Commit => {
                let ids = std::mem::take(&mut *asked.0.lock().unwrap());
                for id in ids {
                    state.wm_workspace_activate_request(id);
                }
            }
            ext_workspace_manager_v1::Request::Stop => {
                state.workspace_list.bound.retain(|b| &b.manager != manager);
                manager.finished();
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, manager: &ExtWorkspaceManagerV1, _: &Asked) {
        state.workspace_list.bound.retain(|b| &b.manager != manager);
    }
}

impl Dispatch<ExtWorkspaceHandleV1, Handle> for Wayluck {
    /// Asking to show it, until the client commits; nothing else is offered.
    fn request(
        _state: &mut Self,
        _client: &Client,
        _handle: &ExtWorkspaceHandleV1,
        request: ext_workspace_handle_v1::Request,
        data: &Handle,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let ext_workspace_handle_v1::Request::Activate = request
            && let Some(asked) = data.manager.data::<Asked>()
        {
            asked.0.lock().unwrap().push(data.id);
        }
    }
}

impl Dispatch<ExtWorkspaceGroupHandleV1, ()> for Wayluck {
    /// Nothing is offered (making workspaces).
    fn request(
        _state: &mut Self,
        _client: &Client,
        _group: &ExtWorkspaceGroupHandleV1,
        _request: ext_workspace_group_handle_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}
