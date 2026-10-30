//! What can be shared, for `wayluck pick`, xdg-desktop-portal-wlr's
//! chooser_cmd: it asks over wayluck_picker_unstable_v1, shows the user its
//! picker, and prints the user's choice for the portal. Nothing is shared by
//! asking: the user decides, in the picker.

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

    /// Someone asks what can be shared: the monitors.
    fn pick(&mut self, pick: WayluckPickV1) {
        for monitor in self.monitors() {
            pick.monitor(monitor.name(), monitor.description());
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
        // Only destroy.
    }
}
