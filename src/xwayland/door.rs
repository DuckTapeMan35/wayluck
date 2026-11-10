//! The X display programs are given (DISPLAY): wayluck's own sockets, its
//! front door. Xwayland starts when the first X program knocks; until it's
//! ready, programs wait at the door. Then each connection is passed through
//! to Xwayland, a thread each way, file descriptors and all (shared memory
//! and GPU buffers go by them). What programs send is read on the way
//! (requests), and what it tells wayluck comes back to it.
//!
//! The door is the only way in: Xwayland lets in only connections with a
//! cookie (MIT-MAGIC-COOKIE-1), and only the door has it. Xwayland reads
//! it from a file in memory (a memfd) only it is given, and each program's
//! connection is given it at its setup. Requests that would let others in
//! the door's way are turned down.

use super::requests::{Heard, Known, Requests, u16_at};
use crate::Wayluck;
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
};
use smithay::reexports::{
    calloop::{
        Interest, LoopHandle, Mode, PostAction,
        channel::{self, Event, Sender},
        generic::Generic,
    },
    x11rb::{
        protocol::xproto::ConnectionExt as _,
        rust_connection::{DefaultStream, RustConnection},
    },
};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, IoSlice, IoSliceMut, Read, Write},
    mem::MaybeUninit,
    net::Shutdown,
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        linux::net::SocketAddrExt,
        unix::net::{SocketAddr, UnixListener, UnixStream},
    },
    thread,
};

/// How many X displays there are to choose from.
pub(super) const DISPLAYS: u32 = 33;

/// The most file descriptors one read brings (X sends a few at a time).
const MOST_FDS: usize = 16;

/// The kind of cookie Xwayland asks for.
const COOKIE_NAME: &[u8] = b"MIT-MAGIC-COOKIE-1";

/// What lets the door in.
#[derive(Clone, Copy)]
struct Key {
    cookie: [u8; 16],
}

/// What comes back to wayluck from the door's threads.
enum Said {
    /// What the door knows of Xwayland, looked up (None: it couldn't be;
    /// programs pass through unread).
    Ready(Option<Known>),
    /// What a program's requests told.
    Heard(Heard),
}

/// The display, and who's waiting at it.
pub struct Door {
    number: u32,
    /// Programs that came before Xwayland was ready.
    waiting: Vec<UnixStream>,
    /// Xwayland's own display, once it's ready.
    xwayland: Option<u32>,
    /// Programs go in (Xwayland's ready, what's known of it looked up).
    open: bool,
    known: Option<Known>,
    /// Xwayland is starting (or ready): it's started once.
    started: bool,
    /// Back to wayluck.
    said: Sender<Said>,
    /// None: Xwayland lets anyone in (no file for its cookie could be
    /// made).
    key: Option<Key>,
    /// The file in memory Xwayland reads the key from, until it's ready:
    /// then only Xwayland has it (it reads it when the first program
    /// comes, and again if it changes; it mustn't).
    authority: Option<OwnedFd>,
    /// Held while the display is wayluck's: the lock file, and the socket's.
    _lock: Lock,
}

/// How to reach Xwayland as the door does (its display, the door's key),
/// from any thread.
#[derive(Clone, Copy)]
pub(super) struct Way {
    xwayland: u32,
    key: Option<Key>,
}

impl Wayluck {
    /// Open wayluck's X display (the first one free), for programs started
    /// from now on (DISPLAY, session_env). Xwayland starts when one connects.
    pub fn open_x_door(&mut self, handle: &LoopHandle<'static, Wayluck>) {
        let opened = (0..DISPLAYS).find_map(|n| {
            let claimed = claim(n)?;
            Some((n, claimed, Lock::take(n)?))
        });
        let Some((number, claimed, lock)) = opened else {
            return tracing::warn!("no X clients: every X display is taken");
        };
        let path = format!("/tmp/.X11-unix/X{number}");
        let listeners = [UnixListener::bind(&path), Ok(claimed)];
        for listener in listeners {
            let listener = match listener.and_then(|l| l.set_nonblocking(true).map(|_| l)) {
                Ok(listener) => listener,
                Err(e) => return tracing::warn!("no X clients: can't listen on :{number}: {e}"),
            };
            let source = Generic::new(listener, Interest::READ, Mode::Level);
            let inserted = handle.insert_source(source, |_, listener, state| {
                while let Ok((stream, _)) = listener.accept() {
                    state.x_knock(stream);
                }
                Ok(PostAction::Continue)
            });
            if let Err(e) = inserted {
                return tracing::warn!("no X clients: {e}");
            }
        }
        let (said, heard) = channel::channel();
        let inserted = handle.insert_source(heard, |event, _, state| match event {
            Event::Msg(Said::Ready(atoms)) => state.x_door_ready(atoms),
            Event::Msg(Said::Heard(Heard::Strut(window, strut))) => {
                state.x_strut_changed(window, strut)
            }
            Event::Closed => {}
        });
        if let Err(e) = inserted {
            return tracing::warn!("no X clients: {e}");
        }
        let made =
            Key::make().inspect_err(|e| tracing::warn!("X programs can go round the door: {e}"));
        let (key, authority) = made.ok().unzip();
        tracing::info!("X programs connect to :{number}");
        self.x_display = Some(format!(":{number}").into());
        self.x_door = Some(Door {
            number,
            waiting: Vec::new(),
            xwayland: None,
            open: false,
            known: None,
            started: false,
            said,
            key,
            authority,
            _lock: lock,
        });
    }

    /// The file in memory Xwayland reads the door's key from, if there's
    /// one (until Xwayland's ready).
    pub(super) fn x_authority(&self) -> Option<BorrowedFd<'_>> {
        Some(self.x_door.as_ref()?.authority.as_ref()?.as_fd())
    }

    /// An X program connected: passed through to Xwayland, once it's ready
    /// (started now, if it isn't yet).
    fn x_knock(&mut self, stream: UnixStream) {
        let Some(door) = &mut self.x_door else {
            return;
        };
        if let (true, Some(xwayland)) = (door.open, door.xwayland) {
            return pass_through(stream, xwayland, door.known, door.key, &door.said);
        }
        door.waiting.push(stream);
        if !door.started {
            door.started = true;
            tracing::info!("an X program at :{}: starting Xwayland", door.number);
            let handle = self.loop_handle.clone();
            self.start_xwayland(&handle);
        }
    }

    /// Xwayland didn't start: those waiting are turned away (their
    /// connections closed), and the next to come tries again.
    pub(super) fn x_door_turn_away(&mut self) {
        if let Some(door) = &mut self.x_door {
            door.waiting.clear();
            door.started = false;
            door.xwayland = None;
        }
    }

    /// Xwayland is ready, on its own display: the file with the key is
    /// its alone from now on; what the door needs to know of it is looked
    /// up (on a thread of its own: Xwayland may be waiting on wayluck),
    /// and then those waiting go in.
    pub(super) fn x_door_open(&mut self, xwayland: u32) {
        let Some(door) = &mut self.x_door else {
            return;
        };
        door.authority = None;
        door.xwayland = Some(xwayland);
        let said = door.said.clone();
        let way = Way {
            xwayland,
            key: door.key,
        };
        let looked_up = thread::Builder::new()
            .name("X lookups".into())
            .spawn(move || said.send(Said::Ready(known(way))));
        if let Err(e) = looked_up {
            tracing::warn!("can't look up what the door needs to know of X: {e}");
            self.x_door_ready(None);
        }
    }

    /// What the door needs to know is looked up: it's open, and those
    /// waiting go in.
    fn x_door_ready(&mut self, known: Option<Known>) {
        let Some(door) = &mut self.x_door else {
            return;
        };
        let Some(xwayland) = door.xwayland else {
            return;
        };
        if known.is_none() {
            tracing::warn!(
                "can't look up what the door needs to know of X: X programs pass through unread"
            );
        }
        door.open = true;
        door.known = known;
        for stream in door.waiting.drain(..) {
            pass_through(stream, xwayland, known, door.key, &door.said);
        }
    }
}

/// Xwayland's numbers for the atoms wayluck reads requests for, and for
/// the extensions whose requests it turns down.
fn known(way: Way) -> Option<Known> {
    let conn = way.connect()?;
    let atom = |name: &str| {
        let reply = conn
            .intern_atom(false, name.as_bytes())
            .ok()?
            .reply()
            .ok()?;
        Some(reply.atom)
    };
    let security = conn.query_extension(b"SECURITY").ok()?.reply().ok()?;
    Some(Known {
        strut: atom("_NET_WM_STRUT")?,
        strut_partial: atom("_NET_WM_STRUT_PARTIAL")?,
        security: security.present.then_some(security.major_opcode),
    })
}

/// An X program's connection, joined to one of its own to Xwayland, with
/// the door's key. What it sends is read on the way, if what the door
/// needs to know of Xwayland is known.
fn pass_through(
    program: UnixStream,
    xwayland: u32,
    known: Option<Known>,
    key: Option<Key>,
    said: &Sender<Said>,
) {
    let server = match reach(xwayland) {
        Ok(server) => server,
        Err(e) => return tracing::warn!("can't reach Xwayland at :{xwayland}: {e}"),
    };
    let (Ok(program_back), Ok(server_back)) = (program.try_clone(), server.try_clone()) else {
        return tracing::warn!("can't pass an X program through");
    };
    let said = said.clone();
    let spawned = [
        thread::Builder::new()
            .name("X program to Xwayland".into())
            .spawn(move || {
                let mut program = program;
                let mut server = server;
                let Ok(little) = let_in(&mut program, &mut server, key.as_ref()) else {
                    // It went before it said who it is: so does this.
                    let _ = server.shutdown(Shutdown::Both);
                    return;
                };
                let reader = known.map(|known| (Requests::new(known, little), said));
                carry(program, server, reader)
            }),
        thread::Builder::new()
            .name("Xwayland to X program".into())
            .spawn(move || carry(server_back, program_back, None)),
    ];
    for spawned in spawned {
        if let Err(e) = spawned {
            tracing::warn!("can't pass an X program through: {e}");
        }
    }
}

/// Everything from comes with, bytes and file descriptors, to to, until
/// from ends: then to ends too (what's read from it, the other thread's).
/// Read on the way by reader, if any, which tells wayluck what it heard
/// and may hold bytes back (a request not yet whole) or change them.
fn carry(from: UnixStream, mut to: UnixStream, mut reader: Option<(Requests, Sender<Said>)>) {
    let mut bytes = vec![0; 64 * 1024];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MOST_FDS))];
    let mut out = Vec::new();
    // File descriptors that came with bytes held back: they go with them.
    let mut held: Vec<OwnedFd> = Vec::new();
    loop {
        let mut control = RecvAncillaryBuffer::new(&mut space);
        let read = recvmsg(
            &from,
            &mut [IoSliceMut::new(&mut bytes)],
            &mut control,
            RecvFlags::CMSG_CLOEXEC,
        );
        let read = match read {
            Ok(read) if read.bytes > 0 => read.bytes,
            _ => break,
        };
        held.extend(control.drain().flat_map(|message| match message {
            RecvAncillaryMessage::ScmRights(fds) => fds.collect(),
            _ => Vec::new(),
        }));
        let going = match &mut reader {
            Some((requests, said)) => {
                out.clear();
                for heard in requests.pass(&bytes[..read], &mut out) {
                    let _ = said.send(Said::Heard(heard));
                }
                &out[..]
            }
            None => &bytes[..read],
        };
        if going.is_empty() {
            continue;
        }
        if send(&to, going, &held).is_err() {
            break;
        }
        held.clear();
    }
    let _ = to.shutdown(Shutdown::Write);
    let _ = to.flush();
}

/// A program's setup (its byte order, protocol version, authorization),
/// on to Xwayland with the door's key in place of whatever it brought:
/// whether its byte order puts the little end first.
fn let_in(
    program: &mut UnixStream,
    server: &mut UnixStream,
    key: Option<&Key>,
) -> io::Result<bool> {
    let mut head = [0; 12];
    program.read_exact(&mut head)?;
    let little = head[0] == b'l';
    // What it brought (each padded to 4): read past.
    let padded = |n: u16| (n as usize).div_ceil(4) * 4;
    let brought = padded(u16_at(&head, 6, little)) + padded(u16_at(&head, 8, little));
    io::copy(&mut program.take(brought as u64), &mut io::sink())?;
    let (name, data): (&[u8], &[u8]) = match key {
        Some(key) => (COOKIE_NAME, &key.cookie),
        None => (&[], &[]),
    };
    let u16_bytes = |n: usize| {
        let n = n as u16;
        if little {
            n.to_le_bytes()
        } else {
            n.to_be_bytes()
        }
    };
    let mut setup = head[..6].to_vec();
    setup.extend(u16_bytes(name.len()));
    setup.extend(u16_bytes(data.len()));
    setup.extend([0, 0]);
    for part in [name, data] {
        setup.extend(part);
        setup.resize(setup.len().next_multiple_of(4), 0);
    }
    server.write_all(&setup)?;
    Ok(little)
}

/// bytes to to, the file descriptors with the first of them.
fn send(to: &UnixStream, bytes: &[u8], fds: &[OwnedFd]) -> io::Result<()> {
    let fds: Vec<BorrowedFd> = fds.iter().map(|fd| fd.as_fd()).collect();
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MOST_FDS))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if !fds.is_empty() {
        control.push(SendAncillaryMessage::ScmRights(&fds));
    }
    let sent = sendmsg(
        to,
        &[IoSlice::new(bytes)],
        &mut control,
        SendFlags::NOSIGNAL,
    )?;
    // The rest (a full socket buffer takes part), without them.
    let mut rest = &bytes[sent..];
    let mut to = to;
    while !rest.is_empty() {
        let sent = to.write(rest)?;
        if sent == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        rest = &rest[sent..];
    }
    Ok(())
}

impl Key {
    /// A new cookie, and an authority file in memory with it (a memfd;
    /// closed if wayluck starts a program).
    fn make() -> io::Result<(Key, OwnedFd)> {
        let mut cookie = [0; 16];
        File::open("/dev/urandom")?.read_exact(&mut cookie)?;
        // One entry, for any host and display (FamilyWild): Xwayland reads
        // it whatever display it's given.
        let field = |bytes: &[u8]| {
            let mut field = (bytes.len() as u16).to_be_bytes().to_vec();
            field.extend(bytes);
            field
        };
        let mut entry = 0xffff_u16.to_be_bytes().to_vec();
        for bytes in [&b""[..], b"", COOKIE_NAME, &cookie] {
            entry.extend(field(bytes));
        }
        let memory = rustix::fs::memfd_create("wayluck-xauth", rustix::fs::MemfdFlags::CLOEXEC)?;
        File::from(memory.try_clone()?).write_all(&entry)?;
        Ok((Key { cookie }, memory))
    }
}

impl Door {
    /// Xwayland's own display, once it's ready.
    pub fn xwayland(&self) -> Option<u32> {
        self.xwayland
    }

    /// How to reach Xwayland, once it's ready.
    pub(super) fn way(&self) -> Option<Way> {
        Some(Way {
            xwayland: self.xwayland?,
            key: self.key,
        })
    }
}

impl Way {
    /// A connection of wayluck's own to Xwayland.
    pub(super) fn connect(self) -> Option<RustConnection> {
        let stream = reach(self.xwayland).ok()?;
        let (stream, _) = DefaultStream::from_unix_stream(stream).ok()?;
        let (name, data) = match self.key {
            Some(key) => (COOKIE_NAME.to_vec(), key.cookie.to_vec()),
            None => (Vec::new(), Vec::new()),
        };
        RustConnection::connect_to_stream_with_auth_info(stream, 0, name, data).ok()
    }
}

/// Display number's abstract socket (the name X servers on Linux listen
/// at, beside their socket file), listened on: if no live process has it,
/// and its lock file is free or its holder gone. Only one process can hold
/// it, and it goes with its holder: no other wayluck takes the display
/// while it's held, whatever lock files are left behind.
pub(super) fn claim(number: u32) -> Option<UnixListener> {
    let claimed = UnixListener::bind_addr(&abstract_name(number).ok()?).ok()?;
    let lock = format!("/tmp/.X{number}-lock");
    let locked = fs::exists(&lock).unwrap_or(true) && !holder_gone(&lock);
    (!locked).then_some(claimed)
}

/// Xwayland, at display number's abstract socket: wayluck claimed it for
/// Xwayland, so it's there whatever becomes of socket files.
fn reach(number: u32) -> io::Result<UnixStream> {
    UnixStream::connect_addr(&abstract_name(number)?)
}

fn abstract_name(number: u32) -> io::Result<SocketAddr> {
    SocketAddr::from_abstract_name(format!("/tmp/.X11-unix/X{number}").as_bytes())
}

/// An X display's lock file (/tmp/.X<n>-lock, the holder's pid in it): the
/// display is wayluck's while it's held. Let go, the files go.
struct Lock {
    number: u32,
}

impl Lock {
    /// Display number's, if it's free (or its holder is gone).
    fn take(number: u32) -> Option<Lock> {
        let path = format!("/tmp/.X{number}-lock");
        let created = OpenOptions::new().write(true).create_new(true).open(&path);
        let mut file = match created {
            Ok(file) => file,
            Err(_) if holder_gone(&path) => {
                fs::remove_file(&path).ok()?;
                return Self::take(number);
            }
            Err(_) => return None,
        };
        let pid = rustix::process::getpid().as_raw_nonzero();
        writeln!(file, "{pid:>10}").ok()?;
        // Its socket may be a dead server's, left behind.
        let _ = fs::remove_file(format!("/tmp/.X11-unix/X{number}"));
        let _ = fs::create_dir_all("/tmp/.X11-unix");
        Some(Lock { number })
    }
}

/// Whether the process a lock file names is gone.
fn holder_gone(path: &str) -> bool {
    let mut pid = String::new();
    let read = File::open(path).and_then(|mut f| f.read_to_string(&mut pid));
    let pid = read.ok().and_then(|_| pid.trim().parse().ok());
    let pid = pid.and_then(rustix::process::Pid::from_raw);
    pid.is_some_and(|pid| rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH))
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(format!("/tmp/.X11-unix/X{}", self.number));
        let _ = fs::remove_file(format!("/tmp/.X{}-lock", self.number));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Displays near the end, which nothing else here takes.

    #[test]
    fn a_display_whose_socket_is_held_isnt_claimed() {
        let held = UnixListener::bind_addr(&abstract_name(32).unwrap()).unwrap();
        assert!(claim(32).is_none());
        drop(held);
        assert!(claim(32).is_some());
    }

    #[test]
    fn a_display_locked_by_a_live_process_isnt_claimed() {
        let lock = "/tmp/.X31-lock";
        let pid = rustix::process::getpid().as_raw_nonzero();
        fs::write(lock, format!("{pid:>10}\n")).unwrap();
        let claimed = claim(31);
        fs::remove_file(lock).unwrap();
        assert!(claimed.is_none());
    }

    #[test]
    fn a_display_locked_by_a_gone_process_is_claimed() {
        let lock = "/tmp/.X30-lock";
        // Past the kernel's largest pid: no process has it.
        fs::write(lock, format!("{:>10}\n", 4_194_305)).unwrap();
        let claimed = claim(30);
        fs::remove_file(lock).unwrap();
        assert!(claimed.is_some());
    }
}
