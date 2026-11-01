//! What wayluck's tests share: a wayluck of their own, headless, driven
//! through its test control socket, and clients to put in it.

#![allow(dead_code)] // each test uses some of it

pub mod wayland;
pub mod x;
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    thread::sleep,
    time::{Duration, Instant},
};

/// How long anything may take before a test gives up on it.
pub const PATIENCE: Duration = Duration::from_secs(10);

/// A wayluck for one test: headless, in a runtime directory of its own
/// (removed after), with no window manager, so a click focuses a window.
pub struct Session {
    child: Child,
    dir: PathBuf,
    control: BufReader<UnixStream>,
    /// The socket's name in dir (wayland-1, say).
    pub wayland_display: String,
    /// Xwayland's (":2", say).
    pub x_display: String,
}

impl Session {
    pub fn start() -> Session {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let n = COUNT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("wayluck-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = std::fs::File::create(dir.join("log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_wayluck"))
            .arg("--headless")
            .env("XDG_RUNTIME_DIR", &dir)
            .env("XDG_CONFIG_HOME", dir.join("config"))
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .env("RUST_LOG", "wayluck=debug")
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("can't start wayluck (built with --features test-harness?)");
        let socket = wait_for(|| control_socket(&dir), "wayluck's test control socket");
        let control = BufReader::new(UnixStream::connect(socket).unwrap());
        let mut session = Session {
            child,
            dir,
            control,
            wayland_display: String::new(),
            x_display: String::new(),
        };
        // Xwayland is ready a moment after wayluck is.
        let (wayland, x) = wait_for(
            || {
                let env = session.command("env");
                let value = |name: &str| {
                    let word = env.split_whitespace().find(|w| w.starts_with(name))?;
                    Some(word[name.len()..].to_string()).filter(|v| !v.is_empty())
                };
                Some((value("WAYLAND_DISPLAY=")?, value("DISPLAY=")?))
            },
            "Xwayland",
        );
        session.wayland_display = wayland;
        session.x_display = x;
        session
    }

    /// The runtime directory (its Wayland socket is in it).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Run a test control command; its answer.
    pub fn command(&mut self, command: &str) -> String {
        let stream = self.control.get_mut();
        stream.write_all(format!("{command}\n").as_bytes()).unwrap();
        let mut answer = String::new();
        self.control.read_line(&mut answer).unwrap();
        let answer = answer.trim().to_string();
        assert!(!answer.starts_with("error"), "{command}: {answer}");
        answer
    }

    /// Click (the left button) at x, y.
    pub fn click(&mut self, x: i32, y: i32) {
        self.command(&format!("move {x} {y}"));
        self.command("button 272 down");
        self.command("button 272 up");
    }

    /// Press and release each key (evdev codes), in turn.
    pub fn type_keys(&mut self, keys: &[u32]) {
        for key in keys {
            self.command(&format!("key {key} down"));
            self.command(&format!("key {key} up"));
        }
    }

    /// What f gives, once it gives something; after PATIENCE, fails the
    /// test, with wayluck's log.
    pub fn wait_for<T>(&self, mut f: impl FnMut() -> Option<T>, what: &str) -> T {
        let start = Instant::now();
        loop {
            if let Some(t) = f() {
                return t;
            }
            if start.elapsed() > PATIENCE {
                panic!("gave up waiting for {what}\nwayluck's log:\n{}", self.log());
            }
            sleep(Duration::from_millis(20));
        }
    }

    /// wayluck's log so far (for a failing test's message).
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("log")).unwrap_or_default()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The test control socket, once wayluck made it.
fn control_socket(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    entries.filter_map(Result::ok).map(|e| e.path()).find(|p| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("wayluck-test."))
    })
}

/// What f gives, once it gives something; panics (naming what) after
/// PATIENCE.
pub fn wait_for<T>(mut f: impl FnMut() -> Option<T>, what: &str) -> T {
    let start = Instant::now();
    loop {
        if let Some(t) = f() {
            return t;
        }
        assert!(start.elapsed() < PATIENCE, "gave up waiting for {what}");
        sleep(Duration::from_millis(20));
    }
}
