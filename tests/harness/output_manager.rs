use super::{Session, wait_for};
use std::{
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_output::Transform, wl_registry::WlRegistry},
};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1,
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};

/// A mode, as told: size and refresh (mHz), and whether it's preferred.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mode {
    pub size: (i32, i32),
    pub refresh: i32,
    pub preferred: bool,
}

/// What it's been told of a head.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Head {
    pub name: String,
    pub enabled: bool,
    pub modes: Vec<Mode>,
    pub mode: Option<Mode>,
    pub position: (i32, i32),
    pub transform: Option<Transform>,
    pub scale: f64,
}

/// What a configuration asks of a head: enabled (with any of these set),
/// or disabled.
#[derive(Clone, Copy, Debug, Default)]
pub struct Wanted {
    pub disabled: bool,
    /// One of its modes, by size.
    pub mode: Option<(i32, i32)>,
    pub custom_mode: Option<(i32, i32, i32)>,
    pub position: Option<(i32, i32)>,
    pub transform: Option<Transform>,
    pub scale: Option<f64>,
}

/// A head: what it's told, its current mode, and whether it's finished.
type Told = (ZwlrOutputHeadV1, Head, Option<ZwlrOutputModeV1>, bool);

#[derive(Default)]
struct Seen {
    heads: Vec<Told>,
    modes: Vec<(ZwlrOutputModeV1, Mode, bool)>,
    serial: Option<u32>,
    /// How each configuration sent turned out, in order.
    outcomes: Vec<Option<&'static str>>,
}

pub struct OutputManager {
    conn: Connection,
    manager: ZwlrOutputManagerV1,
    qh: QueueHandle<Client>,
    seen: Arc<Mutex<Seen>>,
}

struct Client {
    seen: Arc<Mutex<Seen>>,
}

impl OutputManager {
    pub fn open(session: &Session) -> OutputManager {
        let stream = UnixStream::connect(session.dir().join(&session.wayland_display)).unwrap();
        let conn = Connection::from_socket(stream).unwrap();
        let (globals, mut queue) = registry_queue_init::<Client>(&conn).unwrap();
        let qh = queue.handle();
        let manager: ZwlrOutputManagerV1 = globals.bind(&qh, 1..=4, ()).unwrap();
        conn.flush().unwrap();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let mut client = Client { seen: seen.clone() };
        std::thread::spawn(move || while queue.blocking_dispatch(&mut client).is_ok() {});
        let tool = OutputManager {
            conn,
            manager,
            qh,
            seen,
        };
        wait_for(|| tool.seen.lock().unwrap().serial, "the heads to be told");
        tool
    }

    /// The heads it's told of (not finished), once f is true of them.
    pub fn wait_for(&self, what: &str, f: impl Fn(&[Head]) -> bool) -> Vec<Head> {
        wait_for(
            || {
                let heads = self.heads();
                f(&heads).then_some(heads)
            },
            what,
        )
    }

    pub fn heads(&self) -> Vec<Head> {
        let seen = self.seen.lock().unwrap();
        let listed = seen.heads.iter().filter(|(.., finished)| !finished);
        let told = |mode: &ZwlrOutputModeV1| {
            let found = seen.modes.iter().find(|(m, ..)| m == mode);
            found.map(|(_, m, _)| *m)
        };
        listed
            .map(|(head, told_head, current, _)| {
                let mut head_now = told_head.clone();
                head_now.modes = (seen.modes.iter())
                    .filter(|(m, _, finished)| !finished && head_of(m) == Some(head.id()))
                    .map(|(_, m, _)| *m)
                    .collect();
                head_now.mode = current.as_ref().and_then(told);
                head_now
            })
            .collect()
    }

    /// The head called name.
    pub fn head(&self, name: &str) -> Head {
        let heads = self.heads();
        heads.into_iter().find(|h| h.name == name).unwrap()
    }

    /// Send a configuration for the last serial heard: every head as
    /// wanted (one not named: enabled as it is); how it turned out.
    pub fn apply(&self, wanted: &[(&str, Wanted)]) -> &'static str {
        self.configure(wanted, true, None)
    }

    pub fn test(&self, wanted: &[(&str, Wanted)]) -> &'static str {
        self.configure(wanted, false, None)
    }

    /// As apply, made for serial.
    pub fn apply_for(&self, serial: u32, wanted: &[(&str, Wanted)]) -> &'static str {
        self.configure(wanted, true, Some(serial))
    }

    /// The serial last heard.
    pub fn serial(&self) -> u32 {
        self.seen.lock().unwrap().serial.unwrap()
    }

    fn configure(
        &self,
        wanted: &[(&str, Wanted)],
        apply: bool,
        serial: Option<u32>,
    ) -> &'static str {
        let index = {
            let mut seen = self.seen.lock().unwrap();
            let serial = serial.or(seen.serial).unwrap();
            let index = seen.outcomes.len();
            seen.outcomes.push(None);
            let configuration = self.manager.create_configuration(serial, &self.qh, index);
            for (head, told, ..) in seen.heads.iter().filter(|(.., finished)| !finished) {
                let asked = wanted.iter().find(|(name, _)| *name == told.name);
                let asked = asked.map(|(_, w)| *w).unwrap_or_default();
                if asked.disabled {
                    configuration.disable_head(head);
                    continue;
                }
                let configured = configuration.enable_head(head, &self.qh, ());
                if let Some(size) = asked.mode {
                    let mode = (seen.modes.iter())
                        .filter(|(m, _, finished)| !finished && head_of(m) == Some(head.id()))
                        .find(|(_, m, _)| m.size == size);
                    configured.set_mode(&mode.expect("a mode of that size").0);
                }
                if let Some((w, h, refresh)) = asked.custom_mode {
                    configured.set_custom_mode(w, h, refresh);
                }
                if let Some((x, y)) = asked.position {
                    configured.set_position(x, y);
                }
                if let Some(transform) = asked.transform {
                    configured.set_transform(transform);
                }
                if let Some(scale) = asked.scale {
                    configured.set_scale(scale);
                }
            }
            if apply {
                configuration.apply();
            } else {
                configuration.test();
            }
            self.conn.flush().unwrap();
            index
        };
        wait_for(
            || self.seen.lock().unwrap().outcomes[index],
            "the configuration's outcome",
        )
    }
}

/// Which head a mode is of (the head it was introduced by).
fn head_of(mode: &ZwlrOutputModeV1) -> Option<wayland_client::backend::ObjectId> {
    mode.data::<HeadOf>()?.0.lock().unwrap().clone()
}

/// A mode's head, once it's known.
#[derive(Default)]
struct HeadOf(Mutex<Option<wayland_client::backend::ObjectId>>);

impl Dispatch<ZwlrOutputManagerV1, ()> for Client {
    fn event(
        client: &mut Self,
        _: &ZwlrOutputManagerV1,
        event: zwlr_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut seen = client.seen.lock().unwrap();
        match event {
            zwlr_output_manager_v1::Event::Head { head } => {
                seen.heads.push((head, Head::default(), None, false))
            }
            zwlr_output_manager_v1::Event::Done { serial } => seen.serial = Some(serial),
            _ => {}
        }
    }

    wayland_client::event_created_child!(Client, ZwlrOutputManagerV1, [
        zwlr_output_manager_v1::EVT_HEAD_OPCODE => (ZwlrOutputHeadV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputHeadV1, ()> for Client {
    fn event(
        client: &mut Self,
        head: &ZwlrOutputHeadV1,
        event: zwlr_output_head_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_output_head_v1::Event;
        let mut seen = client.seen.lock().unwrap();
        if let Event::Mode { mode } = &event {
            *mode.data::<HeadOf>().unwrap().0.lock().unwrap() = Some(head.id());
            seen.modes.push((mode.clone(), Mode::default(), false));
            return;
        }
        let Some((_, told, current, finished)) = seen.heads.iter_mut().find(|(h, ..)| h == head)
        else {
            return;
        };
        match event {
            Event::Name { name } => told.name = name,
            Event::Enabled { enabled } => told.enabled = enabled != 0,
            Event::CurrentMode { mode } => *current = Some(mode),
            Event::Position { x, y } => told.position = (x, y),
            Event::Transform {
                transform: WEnum::Value(transform),
            } => told.transform = Some(transform),
            Event::Scale { scale } => told.scale = scale,
            Event::Finished => *finished = true,
            _ => {}
        }
    }

    wayland_client::event_created_child!(Client, ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ZwlrOutputModeV1, HeadOf::default()),
    ]);
}

impl Dispatch<ZwlrOutputModeV1, HeadOf> for Client {
    fn event(
        client: &mut Self,
        mode: &ZwlrOutputModeV1,
        event: zwlr_output_mode_v1::Event,
        _: &HeadOf,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_output_mode_v1::Event;
        let mut seen = client.seen.lock().unwrap();
        let Some((_, told, finished)) = seen.modes.iter_mut().find(|(m, ..)| m == mode) else {
            return;
        };
        match event {
            Event::Size { width, height } => told.size = (width, height),
            Event::Refresh { refresh } => told.refresh = refresh,
            Event::Preferred => told.preferred = true,
            Event::Finished => *finished = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, usize> for Client {
    fn event(
        client: &mut Self,
        configuration: &ZwlrOutputConfigurationV1,
        event: zwlr_output_configuration_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_output_configuration_v1::Event;
        let outcome = match event {
            Event::Succeeded => "succeeded",
            Event::Failed => "failed",
            Event::Cancelled => "cancelled",
            _ => return,
        };
        client.seen.lock().unwrap().outcomes[*index] = Some(outcome);
        configuration.destroy();
    }
}

impl Dispatch<ZwlrOutputConfigurationHeadV1, ()> for Client {
    fn event(
        _: &mut Self,
        _: &ZwlrOutputConfigurationHeadV1,
        _: <ZwlrOutputConfigurationHeadV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Client {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
