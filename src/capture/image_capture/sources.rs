//! Sources: what a client may capture, an output or a window (by its
//! handle from the window list).

use crate::{Wayluck, windows::WindowId};
use smithay::{
    output::Output,
    reexports::{
        wayland_protocols::ext::{
            foreign_toplevel_list::v1::server::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
            image_capture_source::v1::server::{
                ext_foreign_toplevel_image_capture_source_manager_v1::{
                    self, ExtForeignToplevelImageCaptureSourceManagerV1,
                },
                ext_image_capture_source_v1::{self, ExtImageCaptureSourceV1},
                ext_output_image_capture_source_manager_v1::{
                    self, ExtOutputImageCaptureSourceManagerV1,
                },
            },
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
};

use super::Source;

impl GlobalDispatch<ExtOutputImageCaptureSourceManagerV1, ()> for Wayluck {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtOutputImageCaptureSourceManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ExtOutputImageCaptureSourceManagerV1, ()> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _manager: &ExtOutputImageCaptureSourceManagerV1,
        request: ext_output_image_capture_source_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let ext_output_image_capture_source_manager_v1::Request::CreateSource {
            source,
            output,
        } = request
        {
            let of = Output::from_resource(&output).map_or(Source::Gone, Source::Output);
            data_init.init(source, of);
        }
    }
}

impl GlobalDispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()> for Wayluck {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtForeignToplevelImageCaptureSourceManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ExtForeignToplevelImageCaptureSourceManagerV1,
        request: ext_foreign_toplevel_image_capture_source_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use ext_foreign_toplevel_image_capture_source_manager_v1::Request;
        if let Request::CreateSource {
            source,
            toplevel_handle,
        } = request
        {
            let of = window_of(&toplevel_handle)
                .filter(|id| state.windows.get(*id).is_some())
                .map_or(Source::Gone, Source::Window);
            data_init.init(source, of);
        }
    }
}

/// The window a handle from the window list is for.
fn window_of(handle: &ExtForeignToplevelHandleV1) -> Option<WindowId> {
    handle.data::<WindowId>().copied()
}

impl Dispatch<ExtImageCaptureSourceV1, Source> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _source: &ExtImageCaptureSourceV1,
        _request: ext_image_capture_source_v1::Request,
        _data: &Source,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // Only destroy: sessions made from it go on.
    }
}
