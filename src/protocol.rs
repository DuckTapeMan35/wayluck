//! Server code generated from protocol/wayluck-wm-unstable-v1.xml: one
//! module per interface (wayluck_wm_manager_v1, ...), with its requests,
//! events and enums.

#![allow(dead_code, non_camel_case_types, unused_imports, clippy::all)]

use wayland_server;
use wayland_server::protocol::*;

pub mod __interfaces {
    use wayland_server::protocol::__interfaces::*;
    wayland_scanner::generate_interfaces!("protocol/wayluck-wm-unstable-v1.xml");
}
use self::__interfaces::*;

wayland_scanner::generate_server_code!("protocol/wayluck-wm-unstable-v1.xml");
