//! The picker's connection to wayluck, made by wayluck (it's the picker's
//! stdin), the only one that sees wayluck_picker_host_v1: what can be shared,
//! and where the user's answer goes.

use crate::protocol::picker::client::wayluck_picker_host_v1::{self, WayluckPickerHostV1};
use std::os::{fd::AsFd, unix::net::UnixStream};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::wl_registry::WlRegistry,
};

pub struct Monitor {
    /// What the portal is told: the wl_output's name.
    pub name: String,
    description: String,
    /// The windows it shows, topmost first, when its window manager
    /// offers no workspaces.
    pub windows: Vec<Window>,
    /// The workspaces on it its window manager offers.
    pub workspaces: Vec<Workspace>,
}

pub struct Workspace {
    /// To capture it by.
    pub id: u32,
    pub name: String,
    /// Its windows, topmost first.
    pub windows: Vec<Window>,
}

pub struct Window {
    /// What the portal is told: its ext-foreign-toplevel-list identifier.
    pub id: u32,
    title: String,
    app_id: String,
}

impl Window {
    /// What the user sees: its title, else its app id.
    pub fn label(&self) -> String {
        match (self.title.is_empty(), self.app_id.is_empty()) {
            (false, _) => self.title.clone(),
            (true, false) => self.app_id.clone(),
            (true, true) => format!("Window {}", self.id),
        }
    }
}

impl Monitor {
    /// What the user sees: its description (which says its name, make and
    /// model), or its name if it has none.
    pub fn label(&self) -> String {
        if self.description.is_empty() {
            self.name.clone()
        } else {
            self.description.clone()
        }
    }
}

/// What wayluck says can be shared, until it says that's all; and whether
/// whoever asked went.
#[derive(Default)]
struct Asking {
    monitors: Vec<Monitor>,
    done: bool,
    closed: bool,
}

/// The monitors the user can share, in wayluck's order, with their
/// workspaces and windows; and the host, to answer by.
pub fn shareable() -> Result<(Vec<Monitor>, Host), Box<dyn std::error::Error>> {
    let given = std::io::stdin().as_fd().try_clone_to_owned()?;
    let connection = Connection::from_socket(UnixStream::from(given)).map_err(not_started)?;
    let (globals, mut queue) = registry_queue_init::<Asking>(&connection).map_err(not_started)?;
    let proxy: WayluckPickerHostV1 = globals
        .bind(&queue.handle(), 1..=1, ())
        .map_err(|_| "wayluck didn't start this as its picker")?;
    let mut asking = Asking::default();
    while !asking.done && !asking.closed {
        queue.blocking_dispatch(&mut asking)?;
    }
    if asking.closed {
        return Err("nobody's asking any more".into());
    }
    let monitors = std::mem::take(&mut asking.monitors);
    let host = Host {
        connection,
        queue,
        proxy,
        asking,
    };
    Ok((monitors, host))
}

/// Its stdin isn't a connection to wayluck: someone ran it by hand.
fn not_started<E>(_: E) -> &'static str {
    "wayluck starts the picker itself (`wayluck pick` asks it to)"
}

/// The picker's side of the question, until it answers. Dropped without
/// answering, it's as if cancelled.
pub struct Host {
    connection: Connection,
    queue: EventQueue<Asking>,
    proxy: WayluckPickerHostV1,
    asking: Asking,
}

impl Host {
    /// The connection, for the thumbnails too (on their own queue).
    pub fn connection(&self) -> Connection {
        self.connection.clone()
    }

    pub fn proxy(&self) -> &WayluckPickerHostV1 {
        &self.proxy
    }

    /// Has whoever asked gone? (Events already read are handled first.)
    pub fn closed(&mut self) -> bool {
        let _ = self.queue.dispatch_pending(&mut self.asking);
        self.asking.closed
    }

    /// The user chose (None: nothing): wayluck hears.
    pub fn answer(self, answer: Option<String>) {
        match answer {
            Some(answer) => self.proxy.choose(answer),
            None => self.proxy.cancel(),
        }
        // Drop: destroyed, and sent.
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.proxy.destroy();
        let _ = self.connection.flush();
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Asking {
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

impl Dispatch<WayluckPickerHostV1, ()> for Asking {
    fn event(
        asking: &mut Self,
        _: &WayluckPickerHostV1,
        event: wayluck_picker_host_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wayluck_picker_host_v1::Event::Done => asking.done = true,
            wayluck_picker_host_v1::Event::Monitor { name, description } => {
                asking.monitors.push(Monitor {
                    name,
                    description,
                    windows: Vec::new(),
                    workspaces: Vec::new(),
                })
            }
            // The last monitor's.
            wayluck_picker_host_v1::Event::Workspace { id, name } => {
                if let Some(monitor) = asking.monitors.last_mut() {
                    let windows = Vec::new();
                    monitor.workspaces.push(Workspace { id, name, windows });
                }
            }
            // The last workspace's, or the last monitor's if it has none.
            wayluck_picker_host_v1::Event::Window { id, title, app_id } => {
                let Some(monitor) = asking.monitors.last_mut() else {
                    return;
                };
                let window = Window { id, title, app_id };
                match monitor.workspaces.last_mut() {
                    Some(workspace) => workspace.windows.push(window),
                    None => monitor.windows.push(window),
                }
            }
            wayluck_picker_host_v1::Event::Closed => asking.closed = true,
        }
    }
}
