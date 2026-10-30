//! What can be shared, for `wayluck pick`, xdg-desktop-portal-wlr's
//! chooser_cmd: it asks over wayluck_picker_unstable_v1, shows the user its
//! picker, and prints the user's choice for the portal. Nothing is shared by
//! asking: the user decides, in the picker.
//! Its thumbnails are captures made small here, so each holds little memory.

use crate::{
    Wayluck,
    image_capture::Source,
    protocol::picker::server::{
        wayluck_pick_v1::{self, WayluckPickV1},
        wayluck_picker_manager_v1::{self, WayluckPickerManagerV1},
    },
    windows,
};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

impl Wayluck {
    pub fn picker_global(dh: &DisplayHandle) {
        dh.create_global::<Wayluck, WayluckPickerManagerV1, _>(1, ());
    }

    /// Someone asks what can be shared: the monitors, each followed by the
    /// windows it shows, topmost first.
    fn pick(&mut self, pick: WayluckPickV1) {
        for monitor in self.monitors() {
            pick.monitor(monitor.name(), monitor.description());
            let Some(scene) = self.scenes.shown_on(monitor) else {
                continue;
            };
            for (id, _) in scene.windows().rev() {
                let Some(toplevel) = self.windows.get(id).and_then(|w| w.toplevel()) else {
                    continue;
                };
                let (title, app_id) = windows::names(toplevel);
                let (title, app_id) = (title.unwrap_or_default(), app_id.unwrap_or_default());
                pick.window(u32::from(id), title, app_id);
            }
        }
        pick.done();
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
        use wayluck_picker_manager_v1::Request;
        match request {
            Request::Pick { id } => {
                let pick = data_init.init(id, ());
                state.pick(pick);
            }
            Request::Thumbnail {
                session,
                source,
                width,
                height,
            } => {
                let session = data_init.init(session, ());
                let source = source.data::<Source>().cloned().unwrap_or(Source::Gone);
                let fit = (width as i32, height as i32).into();
                state.start_session(session, source, false, Some(fit));
            }
            _ => {}
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
        // Only destroy.
    }
}
