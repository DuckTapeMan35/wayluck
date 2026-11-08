//! For tests only
//! drives wayluck through, as a person at the keyboard would. One command a
//! line; each is answered "ok" (or a value) once done, so the test can wait.
//!
//! - `env`: the session's WAYLAND_DISPLAY and DISPLAY (empty until Xwayland
//!   is ready), on one line.
//! - `move X Y`: the pointer to X, Y (logical, on the outputs' layout).
//! - `button CODE down|up`: an evdev button (0x110 the left one, as 272).
//! - `key CODE down|up`: an evdev key code (KEY_A is 30).
//! - `windows`: each window on screen, `title@x,y,widthxheight`, "; "
//!   between.
//! - `x-menus`: each X menu or tooltip, `title@x,y,widthxheight shown` (or
//!   `hidden`), "; " between.
//! - `clipboard`: `x` if what's on the clipboard is an X program's (copied
//!   while X had the keyboard), `-` otherwise.
//! - `picker`: what the picker offers, if it's up, and where its tiles
//!   are: `monitor NAME@x,y,widthxheight` (or `workspace N@…`, `window
//!   ID@…`), ` *` after the selected one, "; " between; `-` if it isn't
//!   up.
//! - `animating`: each window on its way to where it's placed, and where
//!   it's drawn now in its scene, `title@x,y,widthxheight`, "; " between.
//!
//! It's at $XDG_RUNTIME_DIR/wayluck-test.<WAYLAND_DISPLAY>. Builds without
//! the feature have none of this: the harness checks they refuse it.

use crate::{Wayluck, capture::picker::overlay::Choice, windows};
use smithay::{
    backend::{
        input::{ButtonState, InputTime, KeyState},
        renderer::utils::with_renderer_surface_state,
    },
    desktop::Window,
    input::keyboard::Keycode,
    reexports::calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic},
    wayland::{seat::WaylandFocus, selection::data_device::current_data_device_selection_userdata},
};
use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
};

/// evdev codes are XKB's less 8.
const XKB_OFFSET: u32 = 8;

pub fn init(event_loop: &mut EventLoop<Wayluck>, state: &Wayluck) -> std::io::Result<()> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or(std::io::ErrorKind::NotFound)?;
    let mut name = std::ffi::OsString::from("wayluck-test.");
    name.push(&state.socket_name);
    let path = PathBuf::from(dir).join(name);
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    listener.set_nonblocking(true)?;
    tracing::info!("test control at {}", path.display());
    let handle = event_loop.handle();
    let source = Generic::new(listener, Interest::READ, Mode::Level);
    event_loop
        .handle()
        .insert_source(source, move |_, listener, _| {
            // A test connects; its commands are read as they come.
            if let Ok((stream, _)) = listener.accept() {
                let _ = stream.set_nonblocking(true);
                let source = Generic::new(stream, Interest::READ, Mode::Level);
                // What's come of a line not finished yet.
                let mut partial = Vec::new();
                let added = handle.insert_source(source, move |_, stream, state| {
                    Ok(serve(stream, &mut partial, state))
                });
                if let Err(e) = added {
                    tracing::warn!("test control: {e}");
                }
            }
            Ok(PostAction::Continue)
        })
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(())
}

/// Run the commands that have come in full, answering each; Remove once
/// the test hangs up.
fn serve(mut stream: &UnixStream, partial: &mut Vec<u8>, state: &mut Wayluck) -> PostAction {
    let mut buf = [0; 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return PostAction::Remove,
            Ok(n) => partial.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(_) => return PostAction::Remove,
        }
    }
    while let Some(end) = partial.iter().position(|b| *b == b'\n') {
        let line: Vec<u8> = partial.drain(..=end).collect();
        let command = String::from_utf8_lossy(&line);
        let answer = run(command.trim(), state);
        if stream.write_all(format!("{answer}\n").as_bytes()).is_err() {
            return PostAction::Remove;
        }
    }
    PostAction::Continue
}

fn run(command: &str, state: &mut Wayluck) -> String {
    let words: Vec<&str> = command.split_whitespace().collect();
    let time = InputTime::now();
    let pressed = |word: &str| match word {
        "down" => Some(true),
        "up" => Some(false),
        _ => None,
    };
    match words.as_slice() {
        ["env"] => {
            let display = state.x_display.as_deref().unwrap_or_default();
            format!(
                "WAYLAND_DISPLAY={} DISPLAY={}",
                state.socket_name.to_string_lossy(),
                display.to_string_lossy()
            )
        }
        ["windows"] => {
            let windows: Vec<String> = (state.space.elements())
                .filter(|w| drawn(w))
                .filter_map(|w| {
                    let rect = state.space.element_geometry(w)?;
                    let (title, _) = windows::names(w);
                    let (x, y, width, height) = (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h);
                    Some(format!(
                        "{}@{x},{y},{width}x{height}",
                        title.unwrap_or_default()
                    ))
                })
                .collect();
            windows.join("; ")
        }
        // X's menus and tooltips with something drawn, and whether they're
        // shown.
        ["x-menus"] => {
            let shown = state.x_menus_shown();
            let menus: Vec<String> = (state.x_menus.iter())
                .filter(|m| drawn(m))
                .filter_map(|m| {
                    let x11 = m.x11_surface()?;
                    let rect = x11.last_configure();
                    let (x, y, width, height) = (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h);
                    let how = if shown.contains(m) { "shown" } else { "hidden" };
                    Some(format!("{}@{x},{y},{width}x{height} {how}", x11.title()))
                })
                .collect();
            menus.join("; ")
        }
        ["picker"] => match state.picker_tiles() {
            None => "-".into(),
            Some(tiles) => {
                let tiles: Vec<String> = (tiles.into_iter())
                    .map(|(choice, rect, selected)| {
                        let what = match choice {
                            Choice::Monitor(output) => format!("monitor {}", output.name()),
                            Choice::Workspace(scene) => format!("workspace {}", scene.number()),
                            Choice::Window(id) => format!("window {}", u32::from(id)),
                        };
                        let (x, y, w, h) = (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h);
                        let mark = if selected { " *" } else { "" };
                        format!("{what}@{x},{y},{w}x{h}{mark}")
                    })
                    .collect();
                tiles.join("; ")
            }
        },
        ["asking"] => state.picker_askers().to_string(),
        ["clipboard"] => {
            let x = current_data_device_selection_userdata(&state.seat).is_some();
            if x { "x" } else { "-" }.into()
        }
        ["animating"] => {
            let now = std::time::Instant::now();
            let ids: Vec<_> = state.windows.animated(now).collect();
            let windows: Vec<String> = (ids.into_iter())
                .filter_map(|id| {
                    let rect = state.windows.animated_at(id, now)?;
                    // A window closed but still animated shows, untitled.
                    let names = state.windows.get(id).map(windows::names);
                    let title = names.and_then(|(title, _)| title);
                    let (x, y, width, height) = (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h);
                    Some(format!(
                        "{}@{x},{y},{width}x{height}",
                        title.unwrap_or_default()
                    ))
                })
                .collect();
            windows.join("; ")
        }
        ["move", x, y] => match (x.parse(), y.parse()) {
            (Ok(x), Ok(y)) => {
                state.move_pointer((x, y).into(), time);
                "ok".into()
            }
            _ => "error: move X Y".into(),
        },
        ["frames"] => super::headless::FRAMES
            .load(std::sync::atomic::Ordering::Relaxed)
            .to_string(),
        ["button", code, how] => match (code.parse(), pressed(how)) {
            (Ok(code), Some(down)) => {
                let how = if down {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                };
                state.button(code, how, time);
                "ok".into()
            }
            _ => "error: button CODE down|up".into(),
        },
        ["key", code, how] => match (code.parse::<u32>(), pressed(how)) {
            (Ok(code), Some(down)) => {
                let how = if down {
                    KeyState::Pressed
                } else {
                    KeyState::Released
                };
                state.key(Keycode::new(code + XKB_OFFSET), how, time);
                "ok".into()
            }
            _ => "error: key CODE down|up".into(),
        },
        _ => format!("error: unknown command: {command}"),
    }
}

/// Has the window's surface a buffer? Until it has, nothing of it is on
/// screen, and a click there goes through it.
fn drawn(window: &Window) -> bool {
    window.wl_surface().is_some_and(|surface| {
        with_renderer_surface_state(&surface, |s| s.buffer().is_some()).unwrap_or(false)
    })
}
