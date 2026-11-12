//! Workspaces, as programs that list them (bars, pagers) see them: what
//! the window manager says of its own. What they show is up to it
//! (scenes); wayluck only passes on what's said, to X programs as EWMH has
//! it.

use super::protocol::wayluck_wm_workspace_v1::{self, WayluckWmWorkspaceV1};
use crate::Wayluck;
use smithay::{
    output::Output,
    reexports::wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, Resource, backend::ClientId,
    },
};

/// A workspace's id: what the window manager's object for it carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WorkspaceId(u32);

/// What a workspace is said to be.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct State {
    /// Shown on its output.
    pub active: bool,
    /// Something on it wants attention.
    pub urgent: bool,
    /// Better not listed.
    pub hidden: bool,
}

/// What's said of one workspace.
pub struct Workspace {
    pub id: WorkspaceId,
    /// The window manager's object for it.
    object: WayluckWmWorkspaceV1,
    pub name: String,
    pub output: Option<Output>,
    pub state: State,
}

/// What a transaction says of a workspace.
pub enum WorkspaceChange {
    Name(String),
    Output(Option<Output>),
    State(State),
}

/// The window manager's workspaces, in the order it made them.
#[derive(Default)]
pub struct Workspaces {
    listed: Vec<Workspace>,
    /// The last id given: ids aren't given twice, so a gone window
    /// manager's objects never stand for its successor's workspaces.
    last: u32,
}

impl Workspaces {
    /// An id for a new workspace.
    pub fn new_id(&mut self) -> WorkspaceId {
        self.last += 1;
        WorkspaceId(self.last)
    }

    /// List workspace id, the window manager's object, after the others.
    pub fn add(&mut self, id: WorkspaceId, object: WayluckWmWorkspaceV1) {
        self.listed.push(Workspace {
            id,
            object,
            name: String::new(),
            output: None,
            state: State::default(),
        });
    }

    pub fn remove(&mut self, id: WorkspaceId) {
        self.listed.retain(|w| w.id != id);
    }

    /// No longer list any: their window manager's gone.
    pub fn clear(&mut self) {
        self.listed.clear();
    }

    pub fn exists(&self, id: WorkspaceId) -> bool {
        self.listed.iter().any(|w| w.id == id)
    }

    /// Where it is among them, from 0.
    pub fn index(&self, id: WorkspaceId) -> Option<usize> {
        self.listed.iter().position(|w| w.id == id)
    }

    pub fn change(&mut self, id: WorkspaceId, change: WorkspaceChange) {
        let Some(workspace) = self.listed.iter_mut().find(|w| w.id == id) else {
            return;
        };
        match change {
            WorkspaceChange::Name(name) => workspace.name = name,
            WorkspaceChange::Output(output) => workspace.output = output,
            WorkspaceChange::State(state) => workspace.state = state,
        }
    }

    /// All of them, in order.
    pub fn iter(&self) -> impl Iterator<Item = &Workspace> {
        self.listed.iter()
    }

    /// The one at index, from 0.
    pub fn at(&self, index: usize) -> Option<WorkspaceId> {
        self.listed.get(index).map(|w| w.id)
    }

    /// The window manager's object for workspace id.
    pub fn object(&self, id: WorkspaceId) -> Option<&WayluckWmWorkspaceV1> {
        let found = self.listed.iter().find(|w| w.id == id);
        found.map(|w| &w.object)
    }
}

impl Wayluck {
    /// What's said of workspaces changed: whoever lists them hears (X
    /// programs, Wayland bars).
    pub fn workspaces_changed(&mut self) {
        self.x_workspaces_changed();
        self.workspace_list_changed();
    }

    /// Something asked for workspace id to be shown: the window manager
    /// hears so, and decides.
    pub fn wm_workspace_activate_request(&self, id: WorkspaceId) {
        if let Some(object) = self.workspaces.object(id) {
            object.activate_request();
        }
    }
}

pub(super) fn workspace_of(workspace: &WayluckWmWorkspaceV1) -> WorkspaceId {
    *workspace.data::<WorkspaceId>().unwrap()
}

impl Dispatch<WayluckWmWorkspaceV1, WorkspaceId> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _workspace: &WayluckWmWorkspaceV1,
        _request: wayluck_wm_workspace_v1::Request,
        _id: &WorkspaceId,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }

    /// It's no longer listed; windows on it are on none.
    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        _workspace: &WayluckWmWorkspaceV1,
        id: &WorkspaceId,
    ) {
        if state.workspaces.exists(*id) {
            state.workspaces.remove(*id);
            state.workspaces_changed();
        }
    }
}
