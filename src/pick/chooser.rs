//! `wayluck pick`'s question: wayluck asks the user (starting its picker),
//! and this hears the answer, over wayluck_picker_manager_v1. All it ever
//! learns is what the user chose.

use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::wl_registry::WlRegistry,
};

use crate::protocol::picker::client::{
    wayluck_pick_v1::{self, WayluckPickV1},
    wayluck_picker_manager_v1::WayluckPickerManagerV1,
};

/// Some once wayluck answers: Some(answer) chosen, None cancelled.
#[derive(Default)]
struct Asking {
    answer: Option<Option<String>>,
}

/// What the user chose (None: nothing).
pub fn ask() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let connection = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<Asking>(&connection)?;
    let manager: WayluckPickerManagerV1 = globals
        .bind(&queue.handle(), 1..=1, ())
        .map_err(|_| "this compositor isn't wayluck (no wayluck_picker_manager_v1)")?;
    let pick = manager.pick(&queue.handle(), ());
    let mut asking = Asking::default();
    while asking.answer.is_none() {
        queue.blocking_dispatch(&mut asking)?;
    }
    pick.destroy();
    manager.destroy();
    Ok(asking.answer.flatten())
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
        asking.answer = Some(match event {
            wayluck_pick_v1::Event::Chosen { answer } => Some(answer),
            wayluck_pick_v1::Event::Cancelled => None,
        });
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
