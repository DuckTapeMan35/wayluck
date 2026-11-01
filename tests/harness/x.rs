//! X clients for tests: what an X program can do to watch, or take, what
//! isn't its own. Each records what it got.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{JoinHandle, sleep},
    time::Duration,
};
use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        xinput::{self, ConnectionExt as _},
        xproto::{self, ConnectionExt as _},
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

/// Watches for keys every way an X client can without being given them:
/// XInput2 raw key events on the root, key events on a window of its own
/// (which it tries to give the keyboard), and polling the keymap.
pub struct KeySpy {
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl KeySpy {
    /// Watching, on display (":1"), once this returns.
    pub fn start(display: &str) -> KeySpy {
        let (conn, screen) = RustConnection::connect(Some(display)).expect("can't reach Xwayland");
        let root = conn.setup().roots[screen].root;
        // Raw key events, from every device, wherever they go.
        conn.xinput_xi_query_version(2, 2).unwrap().reply().unwrap();
        let mask = xinput::XIEventMask::RAW_KEY_PRESS | xinput::XIEventMask::RAW_KEY_RELEASE;
        conn.xinput_xi_select_events(
            root,
            &[xinput::EventMask {
                deviceid: xinput::Device::ALL.into(),
                mask: vec![mask],
            }],
        )
        .unwrap()
        .check()
        .unwrap();
        // A window of its own, asking for the keyboard.
        let window = conn.generate_id().unwrap();
        let attributes = xproto::CreateWindowAux::new().event_mask(xproto::EventMask::KEY_PRESS);
        conn.create_window(
            0,
            window,
            root,
            0,
            0,
            100,
            100,
            0,
            xproto::WindowClass::INPUT_OUTPUT,
            0,
            &attributes,
        )
        .unwrap();
        conn.map_window(window).unwrap();
        conn.set_input_focus(
            xproto::InputFocus::POINTER_ROOT,
            window,
            x11rb::CURRENT_TIME,
        )
        .unwrap();
        conn.get_input_focus().unwrap().reply().unwrap(); // all done, once answered

        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (stopping, record) = (stop.clone(), seen.clone());
        let thread = std::thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                while let Ok(Some(event)) = conn.poll_for_event() {
                    let what = match event {
                        Event::XinputRawKeyPress(e) => Some(format!("raw key press {}", e.detail)),
                        Event::XinputRawKeyRelease(e) => {
                            Some(format!("raw key release {}", e.detail))
                        }
                        Event::KeyPress(e) => Some(format!("key press {}", e.detail)),
                        _ => None,
                    };
                    record.lock().unwrap().extend(what);
                }
                // Keys held down right now, whoever has the keyboard.
                if let Ok(keymap) = conn.query_keymap().unwrap().reply()
                    && keymap.keys.iter().any(|b| *b != 0)
                {
                    record
                        .lock()
                        .unwrap()
                        .push(format!("keymap {:?}", keymap.keys));
                }
                sleep(Duration::from_millis(2));
            }
        });
        KeySpy {
            stop,
            seen,
            thread: Some(thread),
        }
    }

    /// Stop watching; what it saw.
    pub fn stop(mut self) -> Vec<String> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
        self.seen.lock().unwrap().clone()
    }
}

/// An X window of its own, as an X program would make one: titled, with a
/// class, asking for the keyboard. It records the keys it gets, and
/// whether it has X's input focus.
pub struct XWindow {
    conn: Arc<RustConnection>,
    window: xproto::Window,
    root: xproto::Window,
    stop: Arc<AtomicBool>,
    keys: Arc<Mutex<Vec<u32>>>,
    focused: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl XWindow {
    /// Mapped (shown, once wayluck places it), on display.
    pub fn open(display: &str, title: &str, class: &str, size: (u16, u16)) -> XWindow {
        Self::open_with(display, title, class, size, |_, _| {})
    }

    /// Mapped (shown, once wayluck places it), on display.
    pub fn open_with(
        display: &str,
        title: &str,
        class: &str,
        size: (u16, u16),
        setup: impl FnOnce(&RustConnection, xproto::Window),
    ) -> XWindow {
        let (conn, screen) = RustConnection::connect(Some(display)).expect("can't reach Xwayland");
        let root = conn.setup().roots[screen].root;
        let window = conn.generate_id().unwrap();
        let attributes = xproto::CreateWindowAux::new()
            .background_pixel(0x3366cc)
            .event_mask(xproto::EventMask::KEY_PRESS | xproto::EventMask::FOCUS_CHANGE);
        conn.create_window(
            0,
            window,
            root,
            0,
            0,
            size.0,
            size.1,
            0,
            xproto::WindowClass::INPUT_OUTPUT,
            0,
            &attributes,
        )
        .unwrap();
        let atom = |name: &str| {
            conn.intern_atom(false, name.as_bytes())
                .unwrap()
                .reply()
                .unwrap()
                .atom
        };
        let (net_wm_name, utf8) = (atom("_NET_WM_NAME"), atom("UTF8_STRING"));
        let replace = xproto::PropMode::REPLACE;
        conn.change_property8(
            replace,
            window,
            xproto::AtomEnum::WM_NAME,
            xproto::AtomEnum::STRING,
            title.as_bytes(),
        )
        .unwrap();
        conn.change_property8(replace, window, net_wm_name, utf8, title.as_bytes())
            .unwrap();
        // WM_CLASS: instance, then class, each ending in a nul.
        let wm_class = format!("{}\0{class}\0", class.to_lowercase());
        setup(&conn, window);
        conn.map_window(window).unwrap();
        conn.get_input_focus().unwrap().reply().unwrap(); // all done, once answered
        let conn = Arc::new(conn);
        let polled = conn.clone();
        conn.change_property8(
            replace,
            window,
            xproto::AtomEnum::WM_CLASS,
            xproto::AtomEnum::STRING,
            wm_class.as_bytes(),
        )
        .unwrap();
        conn.map_window(window).unwrap();
        conn.get_input_focus().unwrap().reply().unwrap(); // all done, once answered

        let stop = Arc::new(AtomicBool::new(false));
        let keys = Arc::new(Mutex::new(Vec::new()));
        let focused = Arc::new(AtomicBool::new(false));
        let (stopping, record, has_focus) = (stop.clone(), keys.clone(), focused.clone());
        let thread = std::thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                while let Ok(Some(event)) = polled.poll_for_event() {
                    match event {
                        Event::KeyPress(e) => record.lock().unwrap().push(u32::from(e.detail)),
                        Event::FocusIn(_) => has_focus.store(true, Ordering::Relaxed),
                        Event::FocusOut(_) => has_focus.store(false, Ordering::Relaxed),
                        _ => {}
                    }
                }
                sleep(Duration::from_millis(2));
            }
        });
        XWindow {
            conn,
            window,
            root,
            stop,
            keys,
            focused,
            thread: Some(thread),
        }
    }

    /// Has it X's input focus? Its keys come by X, the click that gave it
    /// focus by Wayland: a key typed before X has said so may go nowhere.
    pub fn focused(&self) -> bool {
        self.focused.load(Ordering::Relaxed)
    }

    /// Its X window id.
    pub fn id(&self) -> xproto::Window {
        self.window
    }

    /// Ask the window manager to make it fullscreen (on), or not, as X
    /// programs do: a _NET_WM_STATE message to the root window.
    pub fn ask_fullscreen(&self, on: bool) {
        let (state, fullscreen) = (
            self.atom("_NET_WM_STATE"),
            self.atom("_NET_WM_STATE_FULLSCREEN"),
        );
        let event = xproto::ClientMessageEvent::new(
            32,
            self.window,
            state,
            [u32::from(on), fullscreen, 0, 1, 0],
        );
        let mask =
            xproto::EventMask::SUBSTRUCTURE_REDIRECT | xproto::EventMask::SUBSTRUCTURE_NOTIFY;
        self.conn.send_event(false, self.root, mask, event).unwrap();
        self.conn.flush().unwrap();
    }

    /// Does its _NET_WM_STATE say it's fullscreen (as the window manager
    /// set it)?
    pub fn is_fullscreen(&self) -> bool {
        let (state, fullscreen) = (
            self.atom("_NET_WM_STATE"),
            self.atom("_NET_WM_STATE_FULLSCREEN"),
        );
        let reply = self
            .conn
            .get_property(false, self.window, state, xproto::AtomEnum::ATOM, 0, 32)
            .unwrap()
            .reply()
            .unwrap();
        reply
            .value32()
            .is_some_and(|mut atoms| atoms.any(|a| a == fullscreen))
    }

    /// Its size, as X has it now.
    pub fn size(&self) -> (u16, u16) {
        let geometry = self
            .conn
            .get_geometry(self.window)
            .unwrap()
            .reply()
            .unwrap();
        (geometry.width, geometry.height)
    }

    fn atom(&self, name: &str) -> xproto::Atom {
        let reply = self
            .conn
            .intern_atom(false, name.as_bytes())
            .unwrap()
            .reply();
        reply.unwrap().atom
    }

    /// The keys it got so far, as evdev codes (X's are 8 more).
    pub fn keys(&self) -> Vec<u32> {
        self.keys.lock().unwrap().iter().map(|k| k - 8).collect()
    }
}

impl Drop for XWindow {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
