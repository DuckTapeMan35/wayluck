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
