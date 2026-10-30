//! Choosing what to share. xdg-desktop-portal-wlr runs `wayluck pick` (its
//! chooser_cmd) when an app asks to share the screen: that asks wayluck, over
//! wayluck_picker_unstable_v1, for the user's choice, and prints it as the
//! portal wants it ("Monitor: NAME" or "Window: ID"). Nothing is shared by
//! asking: the user decides.
//!
//! For now wayluck answers at once with the first monitor: the picker it
//! will show (a window the window manager places, thumbnails of monitors,
//! workspaces and windows) comes next.

use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
};

use crate::{
    Wayluck,
    protocol::picker::server::{
        wayluck_pick_v1::{self, WayluckPickV1},
        wayluck_picker_manager_v1::{self, WayluckPickerManagerV1},
    },
};

impl Wayluck {
    pub fn picker_global(dh: &DisplayHandle) {
        dh.create_global::<Wayluck, WayluckPickerManagerV1, _>(1, ());
    }

    /// Someone asks what to share: the user's answer (for now: the first
    /// monitor).
    fn pick(&mut self, pick: WayluckPickV1) {
        match self.monitors().next() {
            Some(monitor) => pick.chosen(format!("Monitor: {}", monitor.name())),
            None => pick.cancelled(),
        }
    }
}

impl GlobalDispatch<WayluckPickerManagerV1, ()> for Wayluck {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WayluckPickerManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<WayluckPickerManagerV1, ()> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &WayluckPickerManagerV1,
        request: wayluck_picker_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let wayluck_picker_manager_v1::Request::Pick { id } = request {
            let pick = data_init.init(id, ());
            state.pick(pick);
        }
    }
}

impl Dispatch<WayluckPickV1, ()> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _pick: &WayluckPickV1,
        _request: wayluck_pick_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // Only destroy (nothing to close yet).
    }
}

// ---- `wayluck pick` ----

/// `wayluck pick`: ask the wayluck in WAYLAND_DISPLAY what to share, print
/// the answer. The exit code: 0, or 1 if the user cancelled or on an error
/// (said on stderr): the portal shares nothing then.
pub fn run_pick() -> i32 {
    match pick_client::ask() {
        Ok(Some(answer)) => {
            println!("{answer}");
            0
        }
        Ok(None) => 1,
        Err(e) => {
            eprintln!("wayluck pick: {e}");
            1
        }
    }
}

mod pick_client {
    use wayland_client::{
        Connection, Dispatch, QueueHandle,
        globals::{GlobalListContents, registry_queue_init},
        protocol::wl_registry::WlRegistry,
    };

    use crate::protocol::picker::client::{
        wayluck_pick_v1::{self, WayluckPickV1},
        wayluck_picker_manager_v1::WayluckPickerManagerV1,
    };

    /// The answer: Some (what to print), None (cancelled).
    #[derive(Default)]
    struct Asking {
        answer: Option<Option<String>>,
    }

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
            asking.answer = Some(match event {
                wayluck_pick_v1::Event::Chosen { answer } => Some(answer),
                wayluck_pick_v1::Event::Cancelled => None,
            });
        }
    }
}
