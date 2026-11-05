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
    /// Its side of the clipboard (copy, paste).
    clip: Arc<Mutex<Clip>>,
    atoms: Atoms,
    thread: Option<JoinHandle<()>>,
}

/// The atoms copying and pasting take.
#[derive(Clone, Copy)]
struct Atoms {
    clipboard: xproto::Atom,
    utf8: xproto::Atom,
    targets: xproto::Atom,
    /// Where a paste is put, on the window, to be read.
    pasted: xproto::Atom,
}

/// What an X window has copied, and what a paste got.
#[derive(Default)]
struct Clip {
    /// What it hands out as the CLIPBOARD's owner.
    copied: Option<String>,
    /// The answer to its last paste: None, until there is one; then the
    /// text, or None if it was refused.
    pasted: Option<Option<String>>,
    /// How many times it's been asked for what it copied.
    asked: usize,
}

impl XWindow {
    /// Mapped (shown, once wayluck places it), on display.
    pub fn open(display: &str, title: &str, class: &str, size: (u16, u16)) -> XWindow {
        Self::open_with(display, title, class, size, |_, _| {})
    }

    /// Mapped once setup has set it up further (more properties, say).
    pub fn open_with(
        display: &str,
        title: &str,
        class: &str,
        size: (u16, u16),
        setup: impl FnOnce(&RustConnection, xproto::Window),
    ) -> XWindow {
        Self::create(display, title, class, None, size, setup)
    }

    /// A menu, a tooltip: override-redirect, so no window manager places
    /// it; it's shown where it says, at (in X's coordinates, the global
    /// ones), as soon as it's mapped.
    pub fn open_menu(display: &str, title: &str, at: (i16, i16), size: (u16, u16)) -> XWindow {
        Self::create(display, title, "XMenu", Some(at), size, |_, _| {})
    }

    /// A window the window manager places (at None), or an override-redirect
    /// one at a place of its own.
    fn create(
        display: &str,
        title: &str,
        class: &str,
        at: Option<(i16, i16)>,
        size: (u16, u16),
        setup: impl FnOnce(&RustConnection, xproto::Window),
    ) -> XWindow {
        let (conn, screen) = RustConnection::connect(Some(display)).expect("can't reach Xwayland");
        let root = conn.setup().roots[screen].root;
        let window = conn.generate_id().unwrap();
        let attributes = xproto::CreateWindowAux::new()
            .background_pixel(0x3366cc)
            .override_redirect(u32::from(at.is_some()))
            .event_mask(xproto::EventMask::KEY_PRESS | xproto::EventMask::FOCUS_CHANGE);
        let (x, y) = at.unwrap_or_default();
        conn.create_window(
            0,
            window,
            root,
            x,
            y,
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
        conn.change_property8(
            replace,
            window,
            xproto::AtomEnum::WM_CLASS,
            xproto::AtomEnum::STRING,
            wm_class.as_bytes(),
        )
        .unwrap();
        let atoms = Atoms {
            clipboard: atom("CLIPBOARD"),
            utf8,
            targets: atom("TARGETS"),
            pasted: atom("WAYLUCK_TEST_PASTED"),
        };
        setup(&conn, window);
        conn.map_window(window).unwrap();
        conn.get_input_focus().unwrap().reply().unwrap(); // all done, once answered

        let conn = Arc::new(conn);
        let polled = conn.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let keys = Arc::new(Mutex::new(Vec::new()));
        let focused = Arc::new(AtomicBool::new(false));
        let clip = Arc::new(Mutex::new(Clip::default()));
        let (stopping, record, has_focus) = (stop.clone(), keys.clone(), focused.clone());
        let clipped = clip.clone();
        let thread = std::thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                while let Ok(Some(event)) = polled.poll_for_event() {
                    match event {
                        Event::KeyPress(e) => record.lock().unwrap().push(u32::from(e.detail)),
                        Event::FocusIn(_) => has_focus.store(true, Ordering::Relaxed),
                        Event::FocusOut(_) => has_focus.store(false, Ordering::Relaxed),
                        Event::SelectionRequest(e) => {
                            let copied = clipped.lock().unwrap().copied.clone();
                            hand_out(&polled, atoms, &e, copied.as_deref());
                            clipped.lock().unwrap().asked += 1;
                        }
                        Event::SelectionNotify(e) => {
                            let got = received(&polled, window, &e);
                            clipped.lock().unwrap().pasted = Some(got);
                        }
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
            clip,
            atoms,
            thread: Some(thread),
        }
    }

    /// Copy text, as X programs do: own the CLIPBOARD selection, and hand
    /// the text to whoever asks.
    pub fn copy(&self, text: &str) {
        self.clip.lock().unwrap().copied = Some(text.into());
        let (window, clipboard) = (self.window, self.atoms.clipboard);
        (self.conn)
            .set_selection_owner(window, clipboard, x11rb::CURRENT_TIME)
            .unwrap();
        self.conn.get_input_focus().unwrap().reply().unwrap(); // done, once answered
    }

    /// Paste, as X programs do: ask the CLIPBOARD's owner for its text.
    /// None: refused (or nothing to paste).
    pub fn paste(&self) -> Option<String> {
        self.clip.lock().unwrap().pasted = None;
        let a = self.atoms;
        (self.conn)
            .convert_selection(
                self.window,
                a.clipboard,
                a.utf8,
                a.pasted,
                x11rb::CURRENT_TIME,
            )
            .unwrap();
        self.conn.flush().unwrap();
        super::wait_for(
            || self.clip.lock().unwrap().pasted.take(),
            "an answer to the paste",
        )
    }

    /// How many times it's been asked for what it copied (wayluck asks,
    /// to learn what it can be pasted as).
    pub fn asked(&self) -> usize {
        self.clip.lock().unwrap().asked
    }

    /// Does anyone own the CLIPBOARD (a program that copied, or wayluck
    /// for the Wayland clipboard)?
    pub fn clipboard_owned(&self) -> bool {
        let owner = self.conn.get_selection_owner(self.atoms.clipboard).unwrap();
        owner.reply().unwrap().owner != x11rb::NONE
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

    /// The window the window manager says is active (_NET_ACTIVE_WINDOW on
    /// the root window; 0: none).
    pub fn active_window(&self) -> xproto::Window {
        let active = self.atom("_NET_ACTIVE_WINDOW");
        let reply = self
            .conn
            .get_property(false, self.root, active, xproto::AtomEnum::WINDOW, 0, 1)
            .unwrap()
            .reply()
            .unwrap();
        reply.value32().and_then(|mut w| w.next()).unwrap_or(0)
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

/// Answer a request for what an X window copied: its text, if it has any
/// and it's asked for as UTF8_STRING (or what it can give: TARGETS).
fn hand_out(
    conn: &RustConnection,
    atoms: Atoms,
    request: &xproto::SelectionRequestEvent,
    copied: Option<&str>,
) {
    let replace = xproto::PropMode::REPLACE;
    let (requestor, property) = (request.requestor, request.property);
    let answered = match copied {
        Some(text) if request.target == atoms.utf8 => conn
            .change_property8(replace, requestor, property, atoms.utf8, text.as_bytes())
            .is_ok(),
        Some(_) if request.target == atoms.targets => {
            let targets = [atoms.targets, atoms.utf8];
            (conn.change_property32(
                replace,
                requestor,
                property,
                xproto::AtomEnum::ATOM,
                &targets,
            ))
            .is_ok()
        }
        _ => false,
    };
    let notify = xproto::SelectionNotifyEvent {
        response_type: xproto::SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: request.time,
        requestor,
        selection: request.selection,
        target: request.target,
        property: if answered { property } else { x11rb::NONE },
    };
    let _ = conn.send_event(false, requestor, xproto::EventMask::NO_EVENT, notify);
    let _ = conn.flush();
}

/// What a paste got: the text the owner put on window, or None if it
/// refused.
fn received(
    conn: &RustConnection,
    window: xproto::Window,
    notify: &xproto::SelectionNotifyEvent,
) -> Option<String> {
    if notify.property == x11rb::NONE {
        return None;
    }
    let any = xproto::AtomEnum::ANY;
    let reply = (conn.get_property(true, window, notify.property, any, 0, u32::MAX / 4)).ok()?;
    let reply = reply.reply().ok()?;
    Some(String::from_utf8_lossy(&reply.value).into_owned())
}
