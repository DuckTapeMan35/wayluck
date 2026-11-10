//! What X programs are told of their window manager, as EWMH has it: its
//! name, the room left for windows, how far borders reach past windows.
//! wayluck tells what it knows itself, and what the window manager tells
//! it, in X's terms. It's written on Xwayland's root window (and
//! windows) by a connection of wayluck's own, on a thread of its own:
//! Xwayland may be waiting on wayluck.
//!
//! What X programs ask of it the same way (a pager switching desktops) is
//! heard on that connection, on another thread, and passed on to the
//! window manager.

use super::door::Way;
use crate::{Wayluck, windows::WindowId};
use smithay::{
    reexports::{
        calloop::channel::{self, Event},
        x11rb::{
            atom_manager,
            connection::Connection as _,
            errors::{ConnectionError, ReplyError},
            protocol::{
                Event as XEvent,
                xproto::{
                    Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, EventMask,
                    GetPropertyReply, PropMode, Window,
                },
            },
            rust_connection::RustConnection,
            wrapper::ConnectionExt as _,
        },
    },
    utils::{Logical, Rectangle},
};
use std::{
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

/// The window manager's name, until it gives one (and once it's gone).
const OWN_NAME: &str = "wayluck";

atom_manager! {
    Atoms: AtomsCookie {
        _NET_SUPPORTED,
        _NET_SUPPORTING_WM_CHECK,
        _NET_WM_NAME,
        _NET_WORKAREA,
        _NET_FRAME_EXTENTS,
        _NET_NUMBER_OF_DESKTOPS,
        _NET_CURRENT_DESKTOP,
        _NET_DESKTOP_NAMES,
        _NET_DESKTOP_VIEWPORT,
        _NET_WM_DESKTOP,
        UTF8_STRING,
    }
}

/// Something to tell X programs.
enum Hint {
    /// The window manager's name.
    Name(String),
    /// The room left for windows (in X's coordinates: wayluck's).
    WorkArea(Rectangle<i32, Logical>),
    /// How far an X window's border reaches past it: left, right, top,
    /// bottom.
    FrameExtents(Window, [u32; 4]),
    /// The window manager's workspaces, as X's desktops.
    Desktops(Desktops),
}

/// Workspaces, as X has them: numbered desktops.
struct Desktops {
    /// Their names, in order.
    names: Vec<String>,
    /// The one shown.
    current: u32,
    /// Where each one's output is (x, y).
    viewports: Vec<[u32; 2]>,
    /// The X windows said to be on one, and which (ON_ALL: all; None:
    /// nothing is said of it any more).
    windows: Vec<(Window, Option<u32>)>,
}

/// The desktop number that's all of them.
const ON_ALL: u32 = u32::MAX;

/// What an X program asked of its window manager.
enum Asked {
    /// Show desktop (workspace) number.
    Desktop(u32),
    /// Put an X window on desktop number (ON_ALL: all of them).
    WindowDesktop(Window, u32),
}

/// Where hints go: the thread that tells them.
pub struct Hints(Sender<Hint>);

impl Wayluck {
    /// Xwayland is ready: X programs are told from now on, starting with
    /// how things are.
    pub(super) fn x_hints_start(&mut self) {
        let Some(way) = self.x_door.as_ref().and_then(|door| door.way()) else {
            return;
        };
        let (asked, hear_asked) = channel::channel();
        let inserted = self
            .loop_handle
            .insert_source(hear_asked, |event, _, state| {
                if let Event::Msg(asked) = event {
                    state.x_asked(asked);
                }
            });
        if let Err(e) = inserted {
            tracing::warn!("X programs aren't heard asking their window manager: {e}");
        }
        let (hints, heard) = mpsc::channel();
        let told = thread::Builder::new()
            .name("X hints".into())
            .spawn(move || tell(way, heard, asked));
        if let Err(e) = told {
            tracing::warn!("X programs aren't told of their window manager: {e}");
            return;
        }
        self.x_hints = Some(Hints(hints));
        self.x_name_changed();
        self.x_work_area_changed();
        let windows: Vec<_> = self.windows.ids().collect();
        for id in windows {
            self.x_frame_changed(id);
        }
        self.x_workspaces_changed();
    }

    /// The window manager's name changed (or it's gone).
    pub fn x_name_changed(&self) {
        let name = self.wm.name.as_deref().unwrap_or(OWN_NAME);
        self.x_hint(Hint::Name(name.to_string()));
    }

    /// The room left for windows may have changed. EWMH has one area for
    /// all monitors: the first output's.
    pub fn x_work_area_changed(&self) {
        let output = self.space.outputs().next();
        if let Some(area) = output.and_then(|output| self.usable(output)) {
            self.x_hint(Hint::WorkArea(area));
        }
    }

    /// Window id's border changed: if it's an X window, its program hears
    /// how far it reaches (all round, outside it).
    pub fn x_frame_changed(&self, id: WindowId) {
        let Some(x11) = self.windows.get(id).and_then(|w| w.x11_surface().cloned()) else {
            return;
        };
        let width = self.windows.border(id).map_or(0, |b| b.width());
        self.x_hint(Hint::FrameExtents(x11.window_id(), [width; 4]));
    }

    /// The window manager's workspaces changed, or which windows are on
    /// which. The one shown is the first active one.
    pub fn x_workspaces_changed(&self) {
        if self.x_hints.is_none() {
            return;
        }
        let workspaces: Vec<_> = self.workspaces.iter().collect();
        let names = workspaces.iter().map(|w| w.name.clone()).collect();
        let current = workspaces.iter().position(|w| w.state.active);
        let viewports = (workspaces.iter())
            .map(|w| {
                let output = w.output.as_ref();
                let at = output.and_then(|o| self.space.output_geometry(o));
                at.map_or([0, 0], |at| [at.loc.x, at.loc.y].map(|n| n.max(0) as u32))
            })
            .collect();
        let windows = (self.windows.workspaces())
            .filter_map(|(id, workspace)| {
                let x11 = self.windows.get(id)?.x11_surface()?.window_id();
                let desktop = match workspace {
                    None => Some(ON_ALL),
                    Some(workspace) => self.workspaces.index(workspace).map(|n| n as u32),
                };
                Some((x11, desktop))
            })
            .collect();
        self.x_hint(Hint::Desktops(Desktops {
            names,
            current: current.unwrap_or(0) as u32,
            viewports,
            windows,
        }));
    }

    /// An X program asked something of its window manager: it hears so,
    /// in its own terms, if it's something it has.
    fn x_asked(&mut self, asked: Asked) {
        match asked {
            Asked::Desktop(number) => {
                if let Some(workspace) = self.workspaces.at(number as usize) {
                    self.wm_workspace_activate_request(workspace);
                }
            }
            Asked::WindowDesktop(window, number) => {
                let Some(id) = self.windows.find_x11_window(window) else {
                    return;
                };
                let workspace = match number {
                    ON_ALL => None,
                    number => match self.workspaces.at(number as usize) {
                        Some(workspace) => Some(workspace),
                        None => return,
                    },
                };
                self.wm_workspace_request(id, workspace);
            }
        }
    }

    fn x_hint(&self, hint: Hint) {
        if let Some(Hints(hints)) = &self.x_hints {
            // The thread's gone only if X is.
            let _ = hints.send(hint);
        }
    }
}

/// Tell X programs each hint heard, on a connection of its own, until
/// wayluck stops hinting or X goes.
fn tell(way: Way, heard: Receiver<Hint>, asked: channel::Sender<Asked>) {
    let Some(conn) = way.connect() else {
        tracing::warn!("X programs aren't told of their window manager: can't reach Xwayland");
        return;
    };
    if let Err(e) = told(&conn, heard, asked) {
        tracing::debug!("X programs no longer told of their window manager: {e}");
    }
}

fn told(
    conn: &RustConnection,
    heard: Receiver<Hint>,
    asked: channel::Sender<Asked>,
) -> Result<(), ReplyError> {
    let atoms = Atoms::new(conn)?.reply()?;
    let root = conn.setup().roots[0].root;
    // The window that says a window manager's running is smithay's: told
    // after what it supports, and its name. Written to once both are, so
    // they're added to and replaced, not the other way round.
    let check = once_set(conn, root, atoms._NET_SUPPORTING_WM_CHECK)?;
    let Some(check) = check.value32().and_then(|mut v| v.next()) else {
        return Ok(());
    };
    once_set(conn, check, atoms._NET_WM_NAME)?;
    let supported = [
        atoms._NET_WORKAREA,
        atoms._NET_FRAME_EXTENTS,
        atoms._NET_NUMBER_OF_DESKTOPS,
        atoms._NET_CURRENT_DESKTOP,
        atoms._NET_DESKTOP_NAMES,
        atoms._NET_DESKTOP_VIEWPORT,
        atoms._NET_WM_DESKTOP,
    ];
    conn.change_property32(
        PropMode::APPEND,
        root,
        atoms._NET_SUPPORTED,
        AtomEnum::ATOM,
        &supported,
    )?;
    // What's asked of the window manager is sent to the root window, for
    // whoever watches what's on it.
    let watch = ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_NOTIFY);
    conn.change_window_attributes(root, &watch)?;
    conn.flush()?;
    let mut teller = Teller {
        conn,
        atoms,
        root,
        check,
        area: None,
        desktops: 0,
    };
    thread::scope(|scope| {
        scope.spawn(|| {
            if let Err(e) = listen(conn, atoms, root, asked) {
                tracing::debug!("X programs no longer heard asking their window manager: {e}");
            }
        });
        for hint in heard {
            teller.tell(hint)?;
            conn.flush()?;
        }
        Ok(())
    })
}

/// Hear what X programs ask of their window manager, as EWMH has them ask,
/// and pass it on, until X goes (or wayluck stops listening).
fn listen(
    conn: &RustConnection,
    atoms: Atoms,
    root: Window,
    asked: channel::Sender<Asked>,
) -> Result<(), ConnectionError> {
    loop {
        let XEvent::ClientMessage(message) = conn.wait_for_event()? else {
            continue;
        };
        let number = message.data.as_data32()[0];
        let heard = if message.type_ == atoms._NET_CURRENT_DESKTOP && message.window == root {
            Asked::Desktop(number)
        } else if message.type_ == atoms._NET_WM_DESKTOP {
            Asked::WindowDesktop(message.window, number)
        } else {
            continue;
        };
        if asked.send(heard).is_err() {
            return Ok(());
        }
    }
}

/// What tells X programs, and what it's told that's needed again.
struct Teller<'c> {
    conn: &'c RustConnection,
    atoms: Atoms,
    root: Window,
    /// The window that says a window manager's running.
    check: Window,
    /// The room left for windows, last told.
    area: Option<[u32; 4]>,
    /// How many desktops (workspaces) there are, last told.
    desktops: u32,
}

impl Teller<'_> {
    fn tell(&mut self, hint: Hint) -> Result<(), ConnectionError> {
        let a = &self.atoms;
        match hint {
            Hint::Name(name) => {
                self.set8(self.check, a._NET_WM_NAME, a.UTF8_STRING, name.as_bytes())?
            }
            Hint::WorkArea(area) => {
                let area = [area.loc.x, area.loc.y, area.size.w, area.size.h];
                self.area = Some(area.map(|n| n.max(0) as u32));
                self.work_area()?;
            }
            Hint::FrameExtents(window, extents) => self.set32(
                window,
                a._NET_FRAME_EXTENTS,
                AtomEnum::CARDINAL.into(),
                &extents,
            )?,
            Hint::Desktops(desktops) => {
                let root = self.root;
                self.desktops = desktops.names.len() as u32;
                let count = [self.desktops];
                self.set32(
                    root,
                    a._NET_NUMBER_OF_DESKTOPS,
                    AtomEnum::CARDINAL.into(),
                    &count,
                )?;
                // Each name ends with a nul.
                let names: Vec<u8> = (desktops.names.iter())
                    .flat_map(|name| name.bytes().chain([0]))
                    .collect();
                self.set8(root, a._NET_DESKTOP_NAMES, a.UTF8_STRING, &names)?;
                let current = [desktops.current];
                self.set32(
                    root,
                    a._NET_CURRENT_DESKTOP,
                    AtomEnum::CARDINAL.into(),
                    &current,
                )?;
                let viewports = desktops.viewports.concat();
                self.set32(
                    root,
                    a._NET_DESKTOP_VIEWPORT,
                    AtomEnum::CARDINAL.into(),
                    &viewports,
                )?;
                for (window, desktop) in desktops.windows {
                    match desktop {
                        Some(desktop) => {
                            let desktop = [desktop];
                            self.set32(
                                window,
                                a._NET_WM_DESKTOP,
                                AtomEnum::CARDINAL.into(),
                                &desktop,
                            )?
                        }
                        None => self
                            .conn
                            .delete_property(window, a._NET_WM_DESKTOP)?
                            .ignore_error(),
                    }
                }
                self.work_area()?;
            }
        }
        Ok(())
    }

    /// The room left for windows, once for each desktop (EWMH has one
    /// each), or once if there are none.
    fn work_area(&self) -> Result<(), ConnectionError> {
        let Some(area) = self.area else {
            return Ok(());
        };
        let areas = area.repeat(self.desktops.max(1) as usize);
        self.set32(
            self.root,
            self.atoms._NET_WORKAREA,
            AtomEnum::CARDINAL.into(),
            &areas,
        )
    }

    // A window may be gone by now: nothing to tell it.

    fn set32(
        &self,
        window: Window,
        property: Atom,
        kind: Atom,
        values: &[u32],
    ) -> Result<(), ConnectionError> {
        let set = self
            .conn
            .change_property32(PropMode::REPLACE, window, property, kind, values)?;
        set.ignore_error();
        Ok(())
    }

    fn set8(
        &self,
        window: Window,
        property: Atom,
        kind: Atom,
        bytes: &[u8],
    ) -> Result<(), ConnectionError> {
        let set = self
            .conn
            .change_property8(PropMode::REPLACE, window, property, kind, bytes)?;
        set.ignore_error();
        Ok(())
    }
}

/// window's property, once it's set.
fn once_set(
    conn: &RustConnection,
    window: Window,
    property: u32,
) -> Result<GetPropertyReply, ReplyError> {
    let watch = |mask| ChangeWindowAttributesAux::new().event_mask(mask);
    conn.change_window_attributes(window, &watch(EventMask::PROPERTY_CHANGE))?;
    let set = loop {
        let reply = conn
            .get_property(false, window, property, AtomEnum::ANY, 0, 1)?
            .reply()?;
        if reply.type_ != u32::from(AtomEnum::NONE) {
            break reply;
        }
        // Some property of window changed: maybe that one.
        conn.wait_for_event()?;
    };
    conn.change_window_attributes(window, &watch(EventMask::NO_EVENT))?;
    Ok(set)
}
