//! Code generated from protocol/wayluck-picker-unstable-v1.xml, both sides:
//! how `wayluck pick` asks for the user's choice of what to share.

#![allow(dead_code, non_camel_case_types, unused_imports, clippy::all)]

// It makes ext-image-copy-capture sessions, from ext-image-capture-source
// sources: the code made from it uses theirs.
pub mod server {
    use wayland_protocols::ext::{
        image_capture_source::v1::server::*, image_copy_capture::v1::server::*,
    };
    use wayland_server;
    use wayland_server::protocol::*;

    pub mod __interfaces {
        use wayland_protocols::ext::{
            image_capture_source::v1::server::__interfaces::*,
            image_copy_capture::v1::server::__interfaces::*,
        };
        use wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocol/wayluck-picker-unstable-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_server_code!("protocol/wayluck-picker-unstable-v1.xml");
}

pub mod client {
    use wayland_client;
    use wayland_client::protocol::*;
    use wayland_protocols::ext::{
        image_capture_source::v1::client::*, image_copy_capture::v1::client::*,
    };

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        use wayland_protocols::ext::{
            image_capture_source::v1::client::__interfaces::*,
            image_copy_capture::v1::client::__interfaces::*,
        };
        wayland_scanner::generate_interfaces!("protocol/wayluck-picker-unstable-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocol/wayluck-picker-unstable-v1.xml");
}
