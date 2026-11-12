//! A bar's workspaces: an ext-workspace-v1 client recording what it's told
//! of workspaces and their groups, and asking for one to be shown.

use super::{Session, wait_for};
use std::{
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
};
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_output::WlOutput, wl_registry::WlRegistry},
};
use wayland_protocols::ext::workspace::v1::client::{
    ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1},
    ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1},
    ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
};

/// What it's been told of a workspace.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Workspace {
    pub name: String,
    pub active: bool,
    /// Its place among them (its coordinates, one number).
    pub number: Option<u32>,
    /// Whether it's in a group.
    pub grouped: bool,
}

/// What it's been told of a group.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Group {
    /// How many of its outputs it's on.
    pub outputs: usize,
}

#[derive(Default)]
struct Seen {
    workspaces: Vec<(ExtWorkspaceHandleV1, Workspace, bool)>,
    groups: Vec<(ExtWorkspaceGroupHandleV1, Group, bool)>,
}

pub struct Bar {
    conn: Connection,
    manager: ExtWorkspaceManagerV1,
    seen: Arc<Mutex<Seen>>,
}

struct Client {
    seen: Arc<Mutex<Seen>>,
}

impl Bar {
    /// A bar listing session's workspaces. It binds its outputs after the
    /// workspace manager, as bars may.
    pub fn open(session: &Session) -> Bar {
        let stream = UnixStream::connect(session.dir().join(&session.wayland_display)).unwrap();
        let conn = Connection::from_socket(stream).unwrap();
        let (globals, mut queue) = registry_queue_init::<Client>(&conn).unwrap();
        let qh = queue.handle();
        let manager: ExtWorkspaceManagerV1 = globals.bind(&qh, 1..=1, ()).unwrap();
        let _output: WlOutput = globals.bind(&qh, 1..=4, ()).unwrap();
        conn.flush().unwrap();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let mut client = Client { seen: seen.clone() };
        std::thread::spawn(move || while queue.blocking_dispatch(&mut client).is_ok() {});
        Bar {
            conn,
            manager,
            seen,
        }
    }

    /// The workspaces it's told of, in the order it was, once f is true of
    /// them.
    pub fn wait_for(&self, what: &str, f: impl Fn(&[Workspace]) -> bool) -> Vec<Workspace> {
        wait_for(
            || {
                let workspaces = self.workspaces();
                f(&workspaces).then_some(workspaces)
            },
            what,
        )
    }

    /// The workspaces it's told of (not removed).
    pub fn workspaces(&self) -> Vec<Workspace> {
        let seen = self.seen.lock().unwrap();
        let listed = seen.workspaces.iter().filter(|(_, _, removed)| !removed);
        listed.map(|(_, w, _)| w.clone()).collect()
    }

    /// The groups it's told of (not removed).
    pub fn groups(&self) -> Vec<Group> {
        let seen = self.seen.lock().unwrap();
        let listed = seen.groups.iter().filter(|(_, _, removed)| !removed);
        listed.map(|(_, g, _)| g.clone()).collect()
    }

    /// Ask for the workspace called name to be shown.
    pub fn activate(&self, name: &str) {
        let seen = self.seen.lock().unwrap();
        let found = (seen.workspaces.iter()).find(|(_, w, removed)| w.name == name && !removed);
        found.unwrap().0.activate();
        self.manager.commit();
        self.conn.flush().unwrap();
    }
}

impl Dispatch<ExtWorkspaceManagerV1, ()> for Client {
    fn event(
        client: &mut Self,
        _: &ExtWorkspaceManagerV1,
        event: ext_workspace_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut seen = client.seen.lock().unwrap();
        match event {
            ext_workspace_manager_v1::Event::Workspace { workspace } => {
                seen.workspaces
                    .push((workspace, Workspace::default(), false))
            }
            ext_workspace_manager_v1::Event::WorkspaceGroup { workspace_group } => {
                seen.groups.push((workspace_group, Group::default(), false))
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(Client, ExtWorkspaceManagerV1, [
        ext_workspace_manager_v1::EVT_WORKSPACE_GROUP_OPCODE => (ExtWorkspaceGroupHandleV1, ()),
        ext_workspace_manager_v1::EVT_WORKSPACE_OPCODE => (ExtWorkspaceHandleV1, ()),
    ]);
}

impl Dispatch<ExtWorkspaceGroupHandleV1, ()> for Client {
    fn event(
        client: &mut Self,
        group: &ExtWorkspaceGroupHandleV1,
        event: ext_workspace_group_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_workspace_group_handle_v1::Event;
        let mut seen = client.seen.lock().unwrap();
        let grouped = |seen: &mut Seen, workspace: &ExtWorkspaceHandleV1, grouped: bool| {
            if let Some((_, w, _)) = seen.workspaces.iter_mut().find(|(h, ..)| h == workspace) {
                w.grouped = grouped;
            }
        };
        match &event {
            Event::WorkspaceEnter { workspace } => return grouped(&mut seen, workspace, true),
            Event::WorkspaceLeave { workspace } => return grouped(&mut seen, workspace, false),
            _ => {}
        }
        let Some((_, heard, removed)) = seen.groups.iter_mut().find(|(g, ..)| g == group) else {
            return;
        };
        match event {
            Event::OutputEnter { .. } => heard.outputs += 1,
            Event::OutputLeave { .. } => heard.outputs -= 1,
            Event::Removed => *removed = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtWorkspaceHandleV1, ()> for Client {
    fn event(
        client: &mut Self,
        workspace: &ExtWorkspaceHandleV1,
        event: ext_workspace_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_workspace_handle_v1::Event;
        let mut seen = client.seen.lock().unwrap();
        let Some((_, heard, removed)) = seen.workspaces.iter_mut().find(|(w, ..)| w == workspace)
        else {
            return;
        };
        match event {
            Event::Name { name } => heard.name = name,
            Event::State {
                state: WEnum::Value(state),
            } => heard.active = state.contains(ext_workspace_handle_v1::State::Active),
            Event::Coordinates { coordinates } => {
                heard.number = coordinates.try_into().ok().map(u32::from_ne_bytes)
            }
            Event::Removed => *removed = true,
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, ()> for Client {
    fn event(
        _: &mut Self,
        _: &WlOutput,
        _: <WlOutput as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Client {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
