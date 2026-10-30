//! Server code generated from protocol/wayluck-wm-unstable-v1.xml: one
//! module per interface (wayluck_wm_manager_v1, ...), with its requests,
//! events and enums.
//! The picker's protocol (both sides: `wayluck pick` is its client) is in picker.

#![allow(dead_code, non_camel_case_types, unused_imports, clippy::all)]

use wayland_server;
use wayland_server::protocol::*;

pub mod __interfaces {
    use wayland_server::protocol::__interfaces::*;
    wayland_scanner::generate_interfaces!("protocol/wayluck-wm-unstable-v1.xml");
}
use self::__interfaces::*;

wayland_scanner::generate_server_code!("protocol/wayluck-wm-unstable-v1.xml");

/// protocol/wayluck-picker-unstable-v1.xml: how `wayluck pick` asks for the
/// user's choice of what to share (picker.rs).
pub mod picker {
    pub mod server {
        use wayland_server;
        use wayland_server::protocol::*;

        pub mod __interfaces {
            use wayland_server::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("protocol/wayluck-picker-unstable-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_server_code!("protocol/wayluck-picker-unstable-v1.xml");
    }

    pub mod client {
        use wayland_client;
        use wayland_client::protocol::*;

        pub mod __interfaces {
            use wayland_client::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("protocol/wayluck-picker-unstable-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_client_code!("protocol/wayluck-picker-unstable-v1.xml");
    }
}
