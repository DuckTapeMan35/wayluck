//! Everything wayluck keeps, and the socket clients connect to.

use crate::{windows::Windows, wm::WmState};
use smithay::{
    desktop::{Space, Window},
    input::{Seat, SeatState},
    reexports::{
        calloop::{EventLoop, Interest, LoopSignal, Mode, PostAction, generic::Generic},
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
        },
    },
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::xdg::XdgShellState,
        shm::ShmState,
        socket::ListeningSocketSource,
    },
};
use std::{ffi::OsString, sync::Arc};

pub struct Wayluck {
    /// What WAYLAND_DISPLAY should be for clients, e.g.: wayland-1
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,
    /// When wayluck started: frame callbacks carry the time since.
    pub start_time: std::time::Instant,
    /// The window manager, if one is bound.
    pub wm: WmState,
    /// Every client window, in the order they appeared.
    pub windows: Windows,
    /// What's drawn, and where: the outputs, and the windows shown on them.
    pub space: Space<Window>,
    // smithay's state for each global wayluck offers.
    pub compositor_state: CompositorState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub xdg_shell_state: XdgShellState,
    pub data_device_state: DataDeviceState,
    pub seat_state: SeatState<Wayluck>,
    pub seat: Seat<Wayluck>,
}

impl Wayluck {
    pub fn new(event_loop: &mut EventLoop<Self>, display: Display<Self>) -> Self {
        let dh = display.handle();
        let compositor_state = CompositorState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        // xdg-output too: clients asking for outputs' logical size and name.
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let socket_name = listen(display, event_loop);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        let wm = WmState::new(&dh);
        // One keyboard (the default keymap; 200 ms delay, 25 repeats/s) and
        // one pointer, whatever the backend has.
        seat.add_keyboard(Default::default(), 200, 25)
            .expect("the default keymap");
        seat.add_pointer();
        Self {
            socket_name,
            display_handle: dh,
            loop_signal: event_loop.get_signal(),
            compositor_state,
            wm,
            shm_state,
            output_manager_state,
            xdg_shell_state,
            data_device_state,
            seat_state,
            seat,
            windows: Windows::default(),
            start_time: std::time::Instant::now(),
            space: Space::default(),
        }
    }
}

/// Open a client socket (the first free wayland-N) and serve the display
/// from the event loop; the socket's name.
fn listen(mut display: Display<Wayluck>, event_loop: &mut EventLoop<Wayluck>) -> OsString {
    let socket = ListeningSocketSource::new_auto().expect("a wayland socket");
    let name = socket.socket_name().to_os_string();
    let handle = event_loop.handle();
    // A client connecting: wayluck keeps per-client state in ClientState.
    handle
        .insert_source(socket, |stream, _, state| {
            if let Err(e) = state
                .display_handle
                .insert_client(stream, Arc::new(ClientState::default()))
            {
                tracing::warn!("a client couldn't connect: {e}");
            }
        })
        .expect("the socket in the event loop");
    // A client sent something: dispatch it to the handlers. The loop watches
    // a copy of the display's fd; the display itself stays with the callback.
    let fd = display
        .backend()
        .poll_fd()
        .try_clone_to_owned()
        .expect("the display's fd");
    handle
        .insert_source(
            Generic::new(fd, Interest::READ, Mode::Level),
            move |_, _, state| {
                if let Err(e) = display.dispatch_clients(state) {
                    tracing::warn!("dispatching clients: {e}");
                }
                Ok(PostAction::Continue)
            },
        )
        .expect("the display in the event loop");
    name
}

/// What wayluck keeps per client.
#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}
