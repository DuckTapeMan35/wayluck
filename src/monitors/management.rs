use crate::Wayluck;
use smithay::{
    backend::input::InputTime,
    desktop::layer_map_for_output,
    output::{Mode, Output, Scale},
    reexports::{
        wayland_protocols_wlr::output_management::v1::server::{
            zwlr_output_configuration_head_v1::{self, ZwlrOutputConfigurationHeadV1},
            zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
            zwlr_output_head_v1::{self, AdaptiveSyncState, ZwlrOutputHeadV1},
            zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
            zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
            backend::ClientId,
        },
    },
    utils::{Logical, Point, Transform},
};
use std::sync::Mutex;

const VERSION: u32 = 4;

/// A monitor, as clients are told of it.
#[derive(Clone, PartialEq)]
struct Shown {
    modes: Vec<Mode>,
    preferred: Option<Mode>,
    mode: Option<Mode>,
    position: Point<i32, Logical>,
    transform: Transform,
    scale: f64,
    enabled: bool,
}

impl Shown {
    fn of(output: &Output, enabled: bool) -> Shown {
        Shown {
            enabled,
            modes: output.modes(),
            preferred: output.preferred_mode(),
            mode: output.current_mode(),
            position: output.current_location(),
            transform: output.current_transform(),
            scale: output.current_scale().fractional_scale(),
        }
    }
}

/// A monitor as one client has it: its head, its modes', and what it was
/// last told.
struct Head {
    output: Output,
    head: ZwlrOutputHeadV1,
    modes: Vec<(Mode, ZwlrOutputModeV1)>,
    told: Option<Shown>,
}

/// A client's manager, and the monitors it has.
struct Manager {
    manager: ZwlrOutputManagerV1,
    heads: Vec<Head>,
}

/// Every client's manager; the monitors as they were last told, and the
/// serial they were told with.
#[derive(Default)]
pub struct OutputManagement {
    managers: Vec<Manager>,
    shown: Vec<(Output, Shown)>,
    serial: u32,
}

/// What a configuration wants of a monitor it enables (None: as it is).
#[derive(Default)]
struct Wanted {
    mode: Option<Mode>,
    /// A mode not among its own: size and refresh (0: any).
    custom: Option<(i32, i32, i32)>,
    position: Option<Point<i32, Logical>>,
    transform: Option<Transform>,
    scale: Option<f64>,
    adaptive_sync: Option<bool>,
}

/// A configuration head's data: the monitor it's for, and what's wanted.
pub struct HeadWanted {
    output: Output,
    wanted: Mutex<Wanted>,
}

/// A configuration: for which serial, each monitor it configured (None:
/// disabled), and whether it was tested or applied already.
pub struct Configuration {
    serial: u32,
    heads: Mutex<Vec<(Output, Option<ZwlrOutputConfigurationHeadV1>)>>,
    used: Mutex<bool>,
}

/// How a configuration turned out.
enum Outcome {
    Succeeded,
    Failed,
    Cancelled,
}

impl Wayluck {
    pub fn output_management_global(dh: &DisplayHandle) {
        dh.create_global::<Wayluck, ZwlrOutputManagerV1, ()>(VERSION, ());
    }

    /// Something about the monitors may have changed (one came or went,
    /// moved, or shows another mode): if it did, each client hears what,
    /// with a new serial.
    pub fn output_heads_changed(&mut self) {
        let shown: Vec<_> = (self.monitors.iter())
            .map(|m| (m.output.clone(), Shown::of(&m.output, m.enabled())))
            .collect();
        let management = &mut self.output_management;
        if management.shown == shown {
            return;
        }
        management.shown = shown;
        management.serial = management.serial.wrapping_add(1);
        let dh = self.display_handle.clone();
        for manager in &mut management.managers {
            manager.tell(&dh, &management.shown, management.serial);
        }
    }

    /// The configuration's heads, as wanted: tested, or (apply) applied.
    fn configure(&mut self, configuration: &Configuration, apply: bool) -> Outcome {
        if configuration.serial != self.output_management.serial {
            return Outcome::Cancelled;
        }
        let heads = configuration.heads.lock().unwrap();
        let mut changes = Vec::new();
        let mut disabled = Vec::new();
        for (output, head) in heads.iter() {
            let Some(head) = head else {
                disabled.push(output.clone());
                continue;
            };
            let Some(data) = head.data::<HeadWanted>() else {
                return Outcome::Failed;
            };
            let wanted = data.wanted.lock().unwrap();
            match change(output, &wanted, self.set_mode.is_some()) {
                Some(change) => changes.push((output.clone(), change)),
                None => return Outcome::Failed,
            }
        }
        drop(heads);
        if changes.is_empty() {
            return Outcome::Failed;
        }
        if !apply {
            return Outcome::Succeeded;
        }
        let mut changed: Vec<(Output, Mode)> = Vec::new();
        for (output, change) in &changes {
            let Some(old) = output.current_mode() else {
                continue;
            };
            if change.mode == old {
                continue;
            }
            if !self.show_mode(output, change.mode) {
                tracing::warn!("{}: can't show {:?}", output.name(), change.mode);
                for (output, old) in changed {
                    self.show_mode(&output, old);
                }
                return Outcome::Failed;
            }
            changed.push((output.clone(), old));
        }
        for output in &disabled {
            self.enable_monitor(output, false);
        }
        for (output, change) in changes {
            let scale = match change.scale.fract() == 0.0 {
                true => Scale::Integer(change.scale as i32),
                false => Scale::Fractional(change.scale),
            };
            let mode = Some(change.mode);
            output.change_current_state(mode, Some(change.transform), Some(scale), None);
            if let Some(monitor) = self.monitors.iter_mut().find(|m| m.output == output) {
                monitor.placed = Some(change.position);
            }
            tracing::info!(
                "{}: {}x{} at {} mHz, at {:?}, {:?}, scale {}",
                output.name(),
                change.mode.size.w,
                change.mode.size.h,
                change.mode.refresh,
                change.position,
                change.transform,
                change.scale
            );
            self.enable_monitor(&output, true);
        }
        self.monitors_rearranged();
        Outcome::Succeeded
    }

    /// The backend shows output at mode.
    fn show_mode(&self, output: &Output, mode: Mode) -> bool {
        self.set_mode.as_ref().is_some_and(|set| set(output, mode))
    }

    /// The monitors were put elsewhere, or changed size: where everything
    /// goes now.
    fn monitors_rearranged(&mut self) {
        let outputs: Vec<Output> = self.monitors().cloned().collect();
        for output in &outputs {
            // Bars stretch to the new size; windows get what they leave.
            layer_map_for_output(output).arrange();
            self.wm_output_changed(output);
            self.usable_changed(output);
        }
        self.lay_out();
        // The pointer, if it's off them now: back on one.
        let pointer = self.seat.get_pointer().unwrap().current_location();
        let kept = self.keep_on_monitors(pointer, pointer);
        if kept != pointer {
            self.move_pointer(kept, InputTime::now());
        }
        self.sync_space();
        self.x_work_area_changed();
        self.queue_redraw();
    }
}

/// What a monitor becomes.
struct Change {
    mode: Mode,
    position: Point<i32, Logical>,
    transform: Transform,
    scale: f64,
}

/// What output becomes, if it can be what's wanted (other modes: if
/// modes can be set).
fn change(output: &Output, wanted: &Wanted, modes_set: bool) -> Option<Change> {
    let current = output.current_mode()?;
    let mode = match (wanted.mode, wanted.custom) {
        (Some(mode), _) => mode,
        // Only one it has: at that refresh rate, or the fastest.
        (None, Some((w, h, refresh))) => (output.modes().into_iter())
            .filter(|m| (m.size.w, m.size.h) == (w, h))
            .filter(|m| refresh == 0 || m.refresh == refresh)
            .max_by_key(|m| m.refresh)?,
        (None, None) => current,
    };
    if mode != current && !(modes_set && output.modes().contains(&mode)) {
        return None;
    }
    // Adaptive sync isn't done.
    if wanted.adaptive_sync == Some(true) {
        return None;
    }
    Some(Change {
        mode,
        position: wanted.position.unwrap_or(output.current_location()),
        transform: wanted.transform.unwrap_or(output.current_transform()),
        scale: wanted
            .scale
            .unwrap_or(output.current_scale().fractional_scale()),
    })
}

impl Manager {
    /// The monitors as they are now: a head for each new one, what changed
    /// for each it has, finished for each gone; then done, with serial.
    fn tell(&mut self, dh: &DisplayHandle, shown: &[(Output, Shown)], serial: u32) {
        self.heads.retain(|head| {
            let stays = shown.iter().any(|(o, _)| *o == head.output);
            if !stays {
                for (_, mode) in &head.modes {
                    mode.finished();
                }
                head.head.finished();
            }
            stays
        });
        for (output, _) in shown {
            if !self.heads.iter().any(|h| h.output == *output) {
                self.add_head(dh, output);
            }
        }
        for head in &mut self.heads {
            if let Some((_, now)) = shown.iter().find(|(o, _)| *o == head.output) {
                head.tell(dh, now);
            }
        }
        self.manager.done(serial);
    }

    fn add_head(&mut self, dh: &DisplayHandle, output: &Output) {
        let Some(client) = self.manager.client() else {
            return;
        };
        let version = self.manager.version();
        let Ok(head) = client.create_resource::<ZwlrOutputHeadV1, Output, Wayluck>(
            dh,
            version,
            output.clone(),
        ) else {
            return;
        };
        self.manager.head(&head);
        head.name(output.name());
        head.description(output.description());
        let physical = output.physical_properties();
        head.physical_size(physical.size.w, physical.size.h);
        if version >= 2 {
            head.make(physical.make);
            head.model(physical.model);
            head.serial_number(physical.serial_number);
        }
        if version >= 4 {
            head.adaptive_sync(AdaptiveSyncState::Disabled);
        }
        self.heads.push(Head {
            output: output.clone(),
            head,
            modes: Vec::new(),
            told: None,
        });
    }
}

impl Head {
    fn tell(&mut self, dh: &DisplayHandle, now: &Shown) {
        if self.told.as_ref() == Some(now) {
            return;
        }
        if self.told.as_ref().map(|t| t.enabled) != Some(now.enabled) {
            self.head.enabled(now.enabled.into());
        }
        // Enabled again, it's told everything again.
        let told = self.told.as_ref().filter(|t| t.enabled);
        if told == Some(now) {
            return;
        }
        self.modes.retain(|(mode, resource)| {
            let stays = now.modes.contains(mode);
            if !stays {
                resource.finished();
            }
            stays
        });
        for &mode in &now.modes {
            if self.modes.iter().any(|(m, _)| *m == mode) {
                continue;
            }
            let Some(client) = self.head.client() else {
                return;
            };
            let Ok(resource) = client.create_resource::<ZwlrOutputModeV1, _, Wayluck>(
                dh,
                self.head.version(),
                (self.output.clone(), mode),
            ) else {
                continue;
            };
            self.head.mode(&resource);
            resource.size(mode.size.w, mode.size.h);
            resource.refresh(mode.refresh);
            if now.preferred == Some(mode) {
                resource.preferred();
            }
            self.modes.push((mode, resource));
        }
        if !now.enabled {
            self.told = Some(now.clone());
            return;
        }
        if told.map(|t| t.mode) != Some(now.mode) {
            let current = self.modes.iter().find(|(m, _)| Some(*m) == now.mode);
            if let Some((_, resource)) = current {
                self.head.current_mode(resource);
            }
        }
        if told.map(|t| t.position) != Some(now.position) {
            self.head.position(now.position.x, now.position.y);
        }
        if told.map(|t| t.transform) != Some(now.transform) {
            self.head.transform(now.transform.into());
        }
        if told.map(|t| t.scale) != Some(now.scale) {
            self.head.scale(now.scale);
        }
        self.told = Some(now.clone());
    }
}

impl GlobalDispatch<ZwlrOutputManagerV1, ()> for Wayluck {
    fn bind(
        state: &mut Self,
        dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrOutputManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        let mut manager = Manager {
            manager,
            heads: Vec::new(),
        };
        let management = &mut state.output_management;
        manager.tell(dh, &management.shown, management.serial);
        management.managers.push(manager);
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &ZwlrOutputManagerV1,
        request: zwlr_output_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_output_manager_v1::Request;
        match request {
            Request::CreateConfiguration { id, serial } => {
                data_init.init(
                    id,
                    Configuration {
                        serial,
                        heads: Mutex::new(Vec::new()),
                        used: Mutex::new(false),
                    },
                );
            }
            Request::Stop => {
                let managers = &mut state.output_management.managers;
                managers.retain(|m| m.manager != *manager);
                manager.finished();
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, manager: &ZwlrOutputManagerV1, _data: &()) {
        let managers = &mut state.output_management.managers;
        managers.retain(|m| m.manager != *manager);
    }
}

impl Dispatch<ZwlrOutputHeadV1, Output> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        head: &ZwlrOutputHeadV1,
        request: zwlr_output_head_v1::Request,
        _data: &Output,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_output_head_v1::Request::Release = request {
            for manager in &mut state.output_management.managers {
                manager.heads.retain(|h| h.head != *head);
            }
        }
    }
}

impl Dispatch<ZwlrOutputModeV1, (Output, Mode)> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        mode: &ZwlrOutputModeV1,
        request: zwlr_output_mode_v1::Request,
        _data: &(Output, Mode),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_output_mode_v1::Request::Release = request {
            let managers = &mut state.output_management.managers;
            for head in managers.iter_mut().flat_map(|m| &mut m.heads) {
                head.modes.retain(|(_, m)| m != mode);
            }
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, Configuration> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwlrOutputConfigurationV1,
        request: zwlr_output_configuration_v1::Request,
        configuration: &Configuration,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_output_configuration_v1::{Error, Request};
        if matches!(request, Request::Destroy) {
            return;
        }
        if *configuration.used.lock().unwrap() {
            let used = Error::AlreadyUsed;
            return resource.post_error(used, "the configuration was tested or applied already");
        }
        let (head, enabled) = match request {
            Request::EnableHead { id, head } => {
                let output = head.data::<Output>().unwrap().clone();
                let wanted = HeadWanted {
                    output,
                    wanted: Mutex::new(Wanted::default()),
                };
                (head, Some(data_init.init(id, wanted)))
            }
            Request::DisableHead { head } => (head, None),
            Request::Apply | Request::Test => {
                *configuration.used.lock().unwrap() = true;
                // Made before the monitors changed: the client may not have
                // them all.
                if configuration.serial != state.output_management.serial {
                    return resource.cancelled();
                }
                let heads = configuration.heads.lock().unwrap();
                // Every monitor the client has must be configured.
                let unconfigured = (state.output_management.managers.iter())
                    .flat_map(|m| &m.heads)
                    .filter(|h| h.head.client() == resource.client())
                    .find(|h| !heads.iter().any(|(o, _)| *o == h.output));
                if let Some(head) = unconfigured {
                    let error = Error::UnconfiguredHead;
                    let name = head.output.name();
                    return resource.post_error(error, format!("{name} wasn't configured"));
                }
                drop(heads);
                let apply = matches!(request, Request::Apply);
                match state.configure(configuration, apply) {
                    Outcome::Succeeded => resource.succeeded(),
                    Outcome::Failed => resource.failed(),
                    Outcome::Cancelled => resource.cancelled(),
                }
                return;
            }
            _ => return,
        };
        let output = head.data::<Output>().unwrap().clone();
        let mut heads = configuration.heads.lock().unwrap();
        if heads.iter().any(|(o, _)| *o == output) {
            let error = Error::AlreadyConfiguredHead;
            return resource.post_error(error, format!("{} configured twice", output.name()));
        }
        heads.push((output, enabled));
    }
}

impl Dispatch<ZwlrOutputConfigurationHeadV1, HeadWanted> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        resource: &ZwlrOutputConfigurationHeadV1,
        request: zwlr_output_configuration_head_v1::Request,
        data: &HeadWanted,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_output_configuration_head_v1::{Error, Request};
        let mut wanted = data.wanted.lock().unwrap();
        let already = |set: bool| {
            if set {
                resource.post_error(Error::AlreadySet, "set twice");
            }
            set
        };
        match request {
            Request::SetMode { mode } => {
                if already(wanted.mode.is_some() || wanted.custom.is_some()) {
                    return;
                }
                let (output, mode) = mode.data::<(Output, Mode)>().unwrap();
                if *output != data.output {
                    return resource.post_error(Error::InvalidMode, "another monitor's mode");
                }
                wanted.mode = Some(*mode);
            }
            Request::SetCustomMode {
                width,
                height,
                refresh,
            } => {
                if already(wanted.mode.is_some() || wanted.custom.is_some()) {
                    return;
                }
                if width <= 0 || height <= 0 || refresh < 0 {
                    return resource.post_error(Error::InvalidCustomMode, "no such mode");
                }
                wanted.custom = Some((width, height, refresh));
            }
            Request::SetPosition { x, y } => {
                if !already(wanted.position.is_some()) {
                    wanted.position = Some((x, y).into());
                }
            }
            Request::SetTransform { transform } => {
                if already(wanted.transform.is_some()) {
                    return;
                }
                match transform {
                    WEnum::Value(transform) => wanted.transform = Some(transform.into()),
                    WEnum::Unknown(_) => {
                        resource.post_error(Error::InvalidTransform, "no such transform")
                    }
                }
            }
            Request::SetScale { scale } => {
                if already(wanted.scale.is_some()) {
                    return;
                }
                if scale <= 0.0 {
                    return resource.post_error(Error::InvalidScale, "scale must be above 0");
                }
                wanted.scale = Some(scale);
            }
            Request::SetAdaptiveSync { state } => {
                if already(wanted.adaptive_sync.is_some()) {
                    return;
                }
                match state {
                    WEnum::Value(state) => {
                        wanted.adaptive_sync = Some(state == AdaptiveSyncState::Enabled)
                    }
                    WEnum::Unknown(_) => resource.post_error(
                        Error::InvalidAdaptiveSyncState,
                        "no such adaptive sync state",
                    ),
                }
            }
            _ => {}
        }
    }
}
