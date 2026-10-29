//! Everything wayluck keeps, and the socket clients connect to.

use crate::{atomic::Pending, cursor::Cursor, windows::Windows, wm::WmState};
use smithay::{
    backend::allocator::dmabuf::Dmabuf,
    desktop::{LayerSurface, PopupManager, Space, Window},
    input::{Seat, SeatState, keyboard::Keycode},
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, generic::Generic,
            ping::Ping,
        },
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
        },
    },
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        dmabuf::DmabufState,
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::{
            wlr_layer::WlrLayerShellState,
            xdg::{XdgShellState, decoration::XdgDecorationState},
        },
        shm::ShmState,
        socket::ListeningSocketSource,
    },
};
use std::{ffi::OsString, sync::Arc};

/// Switch to the TTY numbered so.
pub type ChangeVt = Box<dyn Fn(i32)>;

/// Whether the renderer can use a client's dmabuf (importing it).
pub type ImportDmabuf = Box<dyn Fn(&Dmabuf) -> bool>;

pub struct Wayluck {
    /// What WAYLAND_DISPLAY should be for clients, e.g.: wayland-1
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,
    /// Asks the backend for a frame (queue_redraw); None until it's set up.
    pub redraw: Option<Ping>,
    /// For timers and idle callbacks (atomic.rs).
    pub loop_handle: LoopHandle<'static, Wayluck>,
    /// When wayluck started: frame callbacks carry the time since.
    pub start_time: std::time::Instant,
    /// The pointer's image, where wayluck draws it (cursor.rs).
    pub cursor: Cursor,
    /// Switches to another TTY (Ctrl+Alt+F-key); None: nested, there's none.
    pub change_vt: Option<ChangeVt>,
    /// Windows' popups (menus, tooltips): shown and hit-tested with them.
    pub popups: PopupManager,
    /// The window manager, if one is bound.
    pub wm: WmState,
    /// Keys held whose press went to the window manager or to wayluck itself: their releases
    /// mustn't reach a client that never saw them pressed.
    pub keys_to_wm: Vec<Keycode>,
    /// A transaction waiting for clients to redraw (atomic.rs).
    pub pending: Option<Pending>,
    /// A launcher or bar holding the keyboard instead of the focused window.
    pub layer_focus: Option<LayerSurface>,
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
    pub xdg_decoration_state: XdgDecorationState,
    pub layer_shell_state: WlrLayerShellState,
    pub dmabuf_state: DmabufState,
    /// The backend's renderer, asked whether it can use a client's dmabuf
    /// (handlers/dmabuf.rs); None until the backend is set up.
    pub import_dmabuf: Option<ImportDmabuf>,
    pub seat_state: SeatState<Wayluck>,
    pub seat: Seat<Wayluck>,
}

impl Wayluck {
    pub fn new(event_loop: &mut EventLoop<'static, Self>, display: Display<Self>) -> Self {
        let dh = display.handle();
        let compositor_state = CompositorState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        // xdg-output too: clients asking for outputs' logical size and name.
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let socket_name = listen(display, event_loop);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        // Clients leave decorations to the compositor (handlers/decoration.rs).
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        // Bars, wallpapers, notifications (handlers/layer_shell.rs).
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
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
            redraw: None,
            loop_handle: event_loop.handle(),
            compositor_state,
            cursor: Cursor::default(),
            change_vt: None,
            popups: PopupManager::default(),
            wm,
            keys_to_wm: Vec::new(),
            pending: None,
            layer_focus: None,
            shm_state,
            output_manager_state,
            xdg_shell_state,
            data_device_state,
            xdg_decoration_state,
            layer_shell_state,
            // Its global is made by the backend, which knows the formats.
            dmabuf_state: DmabufState::new(),
            import_dmabuf: None,
            seat_state,
            seat,
            windows: Windows::default(),
            start_time: std::time::Instant::now(),
            space: Space::default(),
        }
    }

    /// Something on screen changed: draw a frame. However many ask before
    /// it's drawn, it's one.
    pub fn queue_redraw(&self) {
        if let Some(ping) = &self.redraw {
            ping.ping();
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
