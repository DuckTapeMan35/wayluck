//! Asking wayluck what can be shared, over wayluck_picker_unstable_v1.

use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::wl_registry::WlRegistry,
};

use crate::protocol::picker::client::{
    wayluck_pick_v1::{self, WayluckPickV1},
    wayluck_picker_manager_v1::WayluckPickerManagerV1,
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

/// What wayluck says can be shared, until it says that's all.
#[derive(Default)]
struct Asking {
    monitors: Vec<Monitor>,
    done: bool,
}

/// The monitors the user can share, in wayluck's order, with their
/// workspaces and windows.
pub fn shareable() -> Result<Vec<Monitor>, Box<dyn std::error::Error>> {
    let connection = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<Asking>(&connection)?;
    let manager: WayluckPickerManagerV1 = globals
        .bind(&queue.handle(), 1..=1, ())
        .map_err(|_| "this compositor isn't wayluck (no wayluck_picker_manager_v1)")?;
    let pick = manager.pick(&queue.handle(), ());
    let mut asking = Asking::default();
    while !asking.done {
        queue.blocking_dispatch(&mut asking)?;
    }
    pick.destroy();
    manager.destroy();
    Ok(asking.monitors)
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

impl Dispatch<WayluckPickerManagerV1, ()> for Asking {
    fn event(
        _: &mut Self,
        _: &WayluckPickerManagerV1,
        _: <WayluckPickerManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WayluckPickV1, ()> for Asking {
    fn event(
        asking: &mut Self,
        _: &WayluckPickV1,
        event: wayluck_pick_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wayluck_pick_v1::Event::Done => asking.done = true,
            wayluck_pick_v1::Event::Monitor { name, description } => {
                asking.monitors.push(Monitor {
                    name,
                    description,
                    windows: Vec::new(),
                    workspaces: Vec::new(),
                })
            }
            // The last monitor's.
            wayluck_pick_v1::Event::Workspace { id, name } => {
                if let Some(monitor) = asking.monitors.last_mut() {
                    let windows = Vec::new();
                    monitor.workspaces.push(Workspace { id, name, windows });
                }
            }
            // The last workspace's, or the last monitor's if it has none.
            wayluck_pick_v1::Event::Window { id, title, app_id } => {
                let Some(monitor) = asking.monitors.last_mut() else {
                    return;
                };
                let window = Window { id, title, app_id };
                match monitor.workspaces.last_mut() {
                    Some(workspace) => workspace.windows.push(window),
                    None => monitor.windows.push(window),
                }
            }
        }
    }
}
