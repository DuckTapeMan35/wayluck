//! The window manager's side of wayluck: the wayluck_wm_manager_v1 global
//! (protocol/wayluck-wm-unstable-v1.xml). One window manager at a time;
//! whoever binds while one is bound gets `unavailable`.

use crate::{
    Wayluck, image_capture,
    protocol::{
        wayluck_wm_button_binding_v1::{self, WayluckWmButtonBindingV1},
        wayluck_wm_key_binding_v1::{self, WayluckWmKeyBindingV1},
        wayluck_wm_manager_v1::{self, WayluckWmManagerV1},
        wayluck_wm_output_v1::{self, WayluckWmOutputV1},
        wayluck_wm_scene_v1::{self, WayluckWmSceneV1},
        wayluck_wm_transaction_v1::{self, WayluckWmTransactionV1},
        wayluck_wm_window_v1::{self, WayluckWmWindowV1},
    },
    scenes::SceneId,
    windows::{self, Change, WindowId, Windows},
};
use smithay::{
    desktop::{Space, Window, layer_map_for_output},
    input::keyboard::{Keysym, ModifiersState},
    output::{Output, PhysicalProperties, Subpixel},
    reexports::wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, backend::ClientId,
    },
    utils::{Logical, Point, Rectangle},
    wayland::{compositor::with_states, shell::xdg::SurfaceCachedState},
};
use std::{cell::Cell, collections::HashMap, sync::Mutex};

const VERSION: u32 = 1;

/// On a window's surface: its client asked to be fullscreen before its
/// window manager knew the window.
#[derive(Default)]
struct EarlyFullscreen(Cell<bool>);

/// On a window's surface: the size limits its window manager last heard.
#[derive(Default)]
struct SentHints(Cell<Option<[i32; 4]>>);

/// The window manager, if one is bound, and its objects for what wayluck has.
#[derive(Default)]
pub struct WmState {
    pub manager: Option<WayluckWmManagerV1>,
    outputs: Vec<(Output, WayluckWmOutputV1)>,
    windows: HashMap<WindowId, WayluckWmWindowV1>,
    /// The key combinations it has taken from clients.
    bindings: Vec<WayluckWmKeyBindingV1>,
    /// The pointer buttons it has taken from clients.
    buttons: Vec<WayluckWmButtonBindingV1>,
    /// Buttons held whose press went to it (their Linux code), and the
    /// binding that got it: the release goes there too.
    buttons_down: Vec<(u32, WayluckWmButtonBindingV1)>,
    /// It's dragging something: the pointer goes to no client.
    pub pointer_grabbed: bool,
    /// The window it was last told the pointer is over.
    hovered: Option<WindowId>,
}

impl WmState {
    /// Advertise the global.
    pub fn new(dh: &DisplayHandle) -> Self {
        dh.create_global::<Wayluck, WayluckWmManagerV1, ()>(VERSION, ());
        Self::default()
    }

    /// The bound manager and its client, to make objects for.
    fn manager_and_client(&self) -> Option<(WayluckWmManagerV1, Client)> {
        let manager = self.manager.clone()?;
        let client = manager.client()?;
        Some((manager, client))
    }
}

impl GlobalDispatch<WayluckWmManagerV1, ()> for Wayluck {
    fn bind(
        state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WayluckWmManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        if state.wm.manager.as_ref().is_some_and(|m| m.is_alive()) {
            tracing::warn!("a second window manager tried to bind");
            manager.unavailable();
            return;
        }
        tracing::info!("window manager bound");
        state.wm.manager = Some(manager);
        state.wm_announce_all();
    }
}

impl Dispatch<WayluckWmManagerV1, ()> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &WayluckWmManagerV1,
        request: wayluck_wm_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            // Each transaction remembers its manager: only the bound one's count.
            wayluck_wm_manager_v1::Request::Transaction { id } => {
                data_init.init(id, Transaction::new(manager));
            }
            // An unavailable manager's scenes exist, but nothing shows them
            // (its transactions aren't applied).
            wayluck_wm_manager_v1::Request::Scene { id } => {
                data_init.init(id, state.scenes.add());
            }
            wayluck_wm_manager_v1::Request::VirtualOutput {
                id,
                width,
                height,
                template,
                refresh,
            } => {
                // An unavailable manager's (or one of no size) is an output
                // that exists nowhere: no global, nothing shown.
                if state.wm.manager.as_ref() != Some(manager) || width <= 0 || height <= 0 {
                    let nowhere = PhysicalProperties {
                        size: (0, 0).into(),
                        subpixel: Subpixel::Unknown,
                        make: String::new(),
                        model: String::new(),
                    };
                    data_init.init(id, Output::new("NONE".into(), nowhere));
                    return;
                }
                let template = template.and_then(|t| t.data::<Output>().cloned());
                let size = (width, height).into();
                let output = state.add_virtual_output(size, template, refresh);
                let resource = data_init.init(id, output.clone());
                send_output_state(&resource, &output, &state.space);
                state.wm.outputs.push((output, resource));
            }
            wayluck_wm_manager_v1::Request::OfferWorkspace {
                scene,
                output,
                name,
            } => {
                let output = output.data::<Output>().cloned();
                if let (true, Some(output)) = (state.wm.manager.as_ref() == Some(manager), output) {
                    state.offer_workspace(scene, output, name);
                }
            }
            wayluck_wm_manager_v1::Request::OffersDone => {
                if state.wm.manager.as_ref() == Some(manager) {
                    state.offers_done();
                }
            }
            // Handled in destroyed, which also covers the client going away.
            wayluck_wm_manager_v1::Request::Destroy => {}
            wayluck_wm_manager_v1::Request::BindKey {
                id,
                keysym,
                modifiers,
            } => {
                let binding = data_init.init(id, KeyBinding { keysym, modifiers });
                // An unavailable manager's bindings exist, but never match.
                if state.wm.manager.as_ref() == Some(manager) {
                    state.wm.bindings.push(binding);
                }
            }
            wayluck_wm_manager_v1::Request::BindButton {
                id,
                button,
                modifiers,
            } => {
                let binding = data_init.init(id, ButtonBinding { button, modifiers });
                if state.wm.manager.as_ref() == Some(manager) {
                    state.wm.buttons.push(binding);
                }
            }
            wayluck_wm_manager_v1::Request::WarpPointer { x, y } => {
                if state.wm.manager.as_ref() == Some(manager) {
                    let time = state.start_time.elapsed().as_millis() as u32;
                    state.move_pointer((x as f64, y as f64).into(), time);
                }
            }
            wayluck_wm_manager_v1::Request::SetKeymap {
                model,
                layout,
                variant,
                options,
            } => {
                if state.wm.manager.as_ref() == Some(manager) {
                    state.set_keymap(&model, &layout, &variant, &options);
                }
            }
            wayluck_wm_manager_v1::Request::SetLayout { index } => {
                if state.wm.manager.as_ref() == Some(manager) {
                    state.set_layout(index);
                }
            }
            wayluck_wm_manager_v1::Request::GrabPointer => {
                if state.wm.manager.as_ref() == Some(manager) {
                    state.wm.pointer_grabbed = true;
                }
            }
            wayluck_wm_manager_v1::Request::UngrabPointer => {
                if state.wm.manager.as_ref() == Some(manager) {
                    state.wm.pointer_grabbed = false;
                }
            }
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, manager: &WayluckWmManagerV1, _data: &()) {
        if state.wm.manager.as_ref() == Some(manager) {
            tracing::info!("window manager gone");
            // Its objects go with it; windows stay where they are, in wayluck's own scene.
            state.wm = WmState::default();
            state.configure_waiting();
            state.show_pending();
            state.show_own_scene();
        }
    }
}

/// A transaction: its changes, collected until commit.
pub struct Transaction {
    /// Only the bound manager's transactions are applied.
    manager: WayluckWmManagerV1,
    pending: Mutex<Pending>,
}

#[derive(Default)]
struct Pending {
    changes: Vec<Change>,
    committed: bool,
}

// A scene: windows laid out together. Everything about it is
// done in transactions; it only goes.
impl Dispatch<WayluckWmSceneV1, SceneId> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _scene: &WayluckWmSceneV1,
        _request: wayluck_wm_scene_v1::Request,
        _id: &SceneId,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }

    /// Its windows are in no scene any more, and outputs showing it show
    /// nothing. The space follows once the loop is idle: if its window
    /// manager is going away altogether, the windows first move to
    /// wayluck's own scene where they are (manager destroyed).
    fn destroyed(state: &mut Self, _client: ClientId, _scene: &WayluckWmSceneV1, id: &SceneId) {
        state.scenes.remove(*id);
        state.stop_capture_sessions(&image_capture::Source::Scene(*id));
        state.loop_handle.insert_idle(|state| state.sync_space());
    }
}

impl Transaction {
    fn new(manager: &WayluckWmManagerV1) -> Self {
        Self {
            manager: manager.clone(),
            pending: Mutex::default(),
        }
    }
}

impl Dispatch<WayluckWmTransactionV1, Transaction> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        transaction: &WayluckWmTransactionV1,
        request: wayluck_wm_transaction_v1::Request,
        data: &Transaction,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use wayluck_wm_transaction_v1::Request;
        // A change to collect, or None for commit.
        let change = match request {
            Request::Destroy => return,
            Request::Place {
                scene,
                window,
                x,
                y,
                width,
                height,
            } => {
                let rect = Rectangle::new((x, y).into(), (width, height).into());
                Some(Change::Place(scene_of(&scene), id_of(&window), rect))
            }
            Request::SceneSize {
                scene,
                width,
                height,
            } => Some(Change::SceneSize(scene_of(&scene), (width, height).into())),
            Request::Show { output, scene } => {
                let Some(output) = output.data::<Output>().cloned() else {
                    return;
                };
                Some(Change::Show(output, scene.as_ref().map(scene_of)))
            }
            Request::Border {
                window,
                width,
                top,
                right,
                bottom,
                left,
            } => Some(Change::Border(
                id_of(&window),
                width,
                [top, right, bottom, left],
            )),
            Request::Tiled { window, edges } => Some(Change::Tiled(id_of(&window), edges)),
            Request::Fullscreen { window, fullscreen } => {
                Some(Change::Fullscreen(id_of(&window), fullscreen != 0))
            }
            Request::Hide { window } => Some(Change::Hide(id_of(&window))),
            Request::Commit => None,
            Request::Focus { window } => Some(Change::Focus(window.as_ref().map(id_of))),
        };
        let mut pending = data.pending.lock().unwrap();
        if pending.committed {
            transaction.post_error(
                wayluck_wm_transaction_v1::Error::AlreadyCommitted,
                "a request after commit",
            );
            return;
        }
        if let Some(change) = change {
            pending.changes.push(change);
            return;
        }
        pending.committed = true;
        let changes = std::mem::take(&mut pending.changes);
        drop(pending);
        // applied comes from apply, once it's on screen.
        let result = if state.wm.manager.as_ref() == Some(&data.manager) {
            state.apply(changes, Some(transaction.clone()))
        } else {
            Err("not from the window manager")
        };
        if let Err(why) = result {
            transaction.failed(why.into());
        }
    }
}

/// The window a window manager's object stands for.
fn id_of(window: &WayluckWmWindowV1) -> WindowId {
    *window
        .data::<WindowId>()
        .expect("window objects are made with their id")
}

fn scene_of(scene: &WayluckWmSceneV1) -> SceneId {
    *scene.data::<SceneId>().unwrap()
}

/// A key combination the window manager has taken.
pub struct KeyBinding {
    keysym: u32,
    /// The protocol's modifiers bits (modifier_bits).
    modifiers: u32,
}

impl Dispatch<WayluckWmKeyBindingV1, KeyBinding> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _binding: &WayluckWmKeyBindingV1,
        _request: wayluck_wm_key_binding_v1::Request,
        _data: &KeyBinding,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // Only destroy, handled below.
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        binding: &WayluckWmKeyBindingV1,
        _data: &KeyBinding,
    ) {
        state.wm.bindings.retain(|b| b != binding);
    }
}

/// A pointer button combination the window manager has taken.
pub struct ButtonBinding {
    /// Its Linux code.
    button: u32,
    modifiers: u32,
}

impl Dispatch<WayluckWmButtonBindingV1, ButtonBinding> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _binding: &WayluckWmButtonBindingV1,
        _request: wayluck_wm_button_binding_v1::Request,
        _data: &ButtonBinding,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // Only destroy, handled below.
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        binding: &WayluckWmButtonBindingV1,
        _data: &ButtonBinding,
    ) {
        state.wm.buttons.retain(|b| b != binding);
        state.wm.buttons_down.retain(|(_, b)| b != binding);
    }
}

/// Modifiers as the protocol numbers them (X's): Caps and Num Lock left out.
pub fn modifier_bits(m: &ModifiersState) -> u32 {
    use wayluck_wm_manager_v1::Modifiers;
    [
        (m.shift, Modifiers::Shift),
        (m.ctrl, Modifiers::Ctrl),
        (m.alt, Modifiers::Mod1),
        (m.iso_level5_shift, Modifiers::Mod3),
        (m.logo, Modifiers::Mod4),
        (m.iso_level3_shift, Modifiers::Mod5),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .fold(Modifiers::empty(), |all, (_, m)| all | m)
    .bits()
}

// ---- telling the window manager what there is ----
impl Wayluck {
    /// The window manager's binding for a key (any of its keysyms) with
    /// these modifiers, if it has one.
    pub fn wm_binding(&self, syms: &[Keysym], modifiers: u32) -> Option<WayluckWmKeyBindingV1> {
        self.wm
            .bindings
            .iter()
            .find(|b| {
                let k = b
                    .data::<KeyBinding>()
                    .expect("bindings are made with theirs");
                k.modifiers == modifiers && syms.iter().any(|s| s.raw() == k.keysym)
            })
            .cloned()
    }

    /// A button pressed or released: true if it's one the window manager
    /// took (then the client under the pointer mustn't see it).
    pub fn wm_button(&mut self, button: u32, pressed: bool, modifiers: u32) -> bool {
        if !pressed {
            let Some(i) = self.wm.buttons_down.iter().position(|(b, _)| *b == button) else {
                return false;
            };
            let (_, binding) = self.wm.buttons_down.swap_remove(i);
            binding.released();
            return true;
        }
        let found = self.wm.buttons.iter().find(|b| {
            let d = b
                .data::<ButtonBinding>()
                .expect("bindings are made with theirs");
            d.button == button && d.modifiers == modifiers
        });
        let Some(binding) = found.cloned() else {
            return false;
        };
        binding.pressed();
        self.wm.buttons_down.push((button, binding));
        true
    }

    /// The pointer moved: the window manager hears where, and when it's
    /// over another window.
    pub fn wm_pointer_moved(&mut self, at: Point<f64, Logical>, over: Option<WindowId>) {
        let Some(manager) = &self.wm.manager else {
            return;
        };
        manager.pointer_position(at.x.floor() as i32, at.y.floor() as i32);
        if over == self.wm.hovered {
            return;
        }
        if let Some(r) = self.wm.hovered.and_then(|id| self.wm.windows.get(&id)) {
            r.pointer_leave();
        }
        if let Some(r) = over.and_then(|id| self.wm.windows.get(&id)) {
            r.pointer_enter();
        }
        self.wm.hovered = over;
    }

    /// A window manager was just accepted: every output, every window, then ready.
    fn wm_announce_all(&mut self) {
        let outputs: Vec<Output> = self.monitors().cloned().collect();
        for output in outputs {
            self.wm_output_added(&output);
        }
        // Nothing is shown until it says what (its first transaction).
        self.scenes.show_nothing();
        self.sync_space();
        // Windows that have committed; the rest are announced at their first commit.
        let ids: Vec<WindowId> = self
            .windows
            .ids()
            .filter(|&id| {
                (self.windows.get(id).and_then(|w| w.toplevel()))
                    .is_some_and(|t| t.is_initial_configure_sent())
            })
            .collect();
        for id in ids {
            self.wm_window_added(id);
        }
        if let Some(m) = &self.wm.manager {
            m.ready();
        }
    }

    /// A virtual output made to share a workspace: the window
    /// manager gets it, and hears which scene it shows. false: there's no
    /// window manager to.
    pub fn wm_share_workspace(&mut self, output: &Output, scene: &WayluckWmSceneV1) -> bool {
        let Some((manager, client)) = self.wm.manager_and_client() else {
            return false;
        };
        let Ok(resource) = client.create_resource::<WayluckWmOutputV1, Output, Wayluck>(
            &self.display_handle,
            manager.version(),
            output.clone(),
        ) else {
            return false;
        };
        manager.share_workspace(&resource, scene);
        send_output_state(&resource, output, &self.space);
        self.wm.outputs.push((output.clone(), resource));
        true
    }

    /// Tell the window manager (if any) about an output.
    pub fn wm_output_added(&mut self, output: &Output) {
        let Some((manager, client)) = self.wm.manager_and_client() else {
            return;
        };
        let Ok(resource) = client.create_resource::<WayluckWmOutputV1, Output, Wayluck>(
            &self.display_handle,
            manager.version(),
            output.clone(),
        ) else {
            return;
        };
        manager.output(&resource);
        send_output_state(&resource, output, &self.space);
        self.wm.outputs.push((output.clone(), resource));
    }

    /// An output moved or changed size: the window manager hears the new geometry.
    pub fn wm_output_changed(&mut self, output: &Output) {
        for (o, resource) in &self.wm.outputs {
            if o == output {
                send_output_state(resource, output, &self.space);
            }
        }
    }

    /// Tell the window manager (if any) about a window, once, with what its
    /// client set up so far: its names, parent, size limits, and whether it
    /// asked to be fullscreen.
    pub fn wm_window_added(&mut self, id: WindowId) {
        if self.wm.windows.contains_key(&id) {
            return;
        }
        let Some((manager, client)) = self.wm.manager_and_client() else {
            return;
        };
        let Ok(resource) = client.create_resource::<WayluckWmWindowV1, WindowId, Wayluck>(
            &self.display_handle,
            manager.version(),
            id,
        ) else {
            return;
        };
        manager.window(&resource, id.into());
        self.wm.windows.insert(id, resource);

        self.wm_window_title(id);
        self.wm_window_app_id(id);
        self.wm_window_parent(id);
        self.wm_size_hints(id);
        let toplevel = self.windows.get(id).and_then(|w| w.toplevel());
        let early = toplevel.is_some_and(|t| {
            with_states(t.wl_surface(), |s| {
                s.data_map
                    .get::<EarlyFullscreen>()
                    .is_some_and(|f| f.0.get())
            })
        });
        if early {
            self.wm_fullscreen_request(id, true);
        }
    }

    /// A window's parent changed: the window manager hears the new one
    /// (none, if the parent isn't a window it knows).
    pub fn wm_window_parent(&self, id: WindowId) {
        let Some(resource) = self.wm.windows.get(&id) else {
            return;
        };
        let parent = (self.windows.get(id).and_then(|w| w.toplevel()))
            .and_then(|t| t.parent())
            .and_then(|surface| Windows::id_of(&surface))
            .and_then(|parent| self.wm.windows.get(&parent));
        resource.parent(parent);
    }

    /// A window's size limits, if they changed since the window manager
    /// last heard them.
    pub fn wm_size_hints(&self, id: WindowId) {
        let Some(resource) = self.wm.windows.get(&id) else {
            return;
        };
        let Some(toplevel) = self.windows.get(id).and_then(|w| w.toplevel()) else {
            return;
        };
        with_states(toplevel.wl_surface(), |s| {
            let limits = *s.cached_state.get::<SurfaceCachedState>().current();
            let hints = [
                limits.min_size.w,
                limits.min_size.h,
                limits.max_size.w,
                limits.max_size.h,
            ];
            s.data_map.insert_if_missing(SentHints::default);
            let sent = &s.data_map.get::<SentHints>().unwrap().0;
            if sent.replace(Some(hints)) != Some(hints) {
                resource.size_hints(hints[0], hints[1], hints[2], hints[3]);
            }
        });
    }

    /// A window's title changed: the window manager hears the new one.
    pub fn wm_window_title(&mut self, id: WindowId) {
        if let Some((resource, (Some(title), _))) = self.wm_window_names(id) {
            resource.title(title);
        }
    }

    /// A window's app id changed: the window manager hears the new one.
    pub fn wm_window_app_id(&mut self, id: WindowId) {
        if let Some((resource, (_, Some(app_id)))) = self.wm_window_names(id) {
            resource.app_id(app_id);
        }
    }

    /// The window manager's object for a window, and the window's names.
    fn wm_window_names(&self, id: WindowId) -> Option<(&WayluckWmWindowV1, windows::Names)> {
        let resource = self.wm.windows.get(&id)?;
        let toplevel = self.windows.get(id)?.toplevel()?;
        Some((resource, windows::names(toplevel)))
    }

    /// The user clicked a window: the window manager hears so, and decides
    /// about focus.
    pub fn wm_focus_request(&self, id: WindowId) {
        if let Some(resource) = self.wm.windows.get(&id) {
            resource.focus_request();
        }
    }

    /// A window's client asked to be fullscreen, or to stop: the window
    /// manager hears so, and decides. false: there's none to ask.
    pub fn wm_fullscreen_request(&self, id: WindowId, on: bool) -> bool {
        let Some(resource) = self.wm.windows.get(&id) else {
            // Before its first commit: the window manager hears it with the window.
            let toplevel = self.windows.get(id).and_then(|w| w.toplevel());
            let (Some(toplevel), Some(_)) = (toplevel, &self.wm.manager) else {
                return false;
            };
            with_states(toplevel.wl_surface(), |s| {
                s.data_map.insert_if_missing(EarlyFullscreen::default);
                s.data_map.get::<EarlyFullscreen>().unwrap().0.set(on);
            });
            return true;
        };
        if on {
            resource.fullscreen_request();
        } else {
            resource.unfullscreen_request();
        }
        true
    }

    /// Something started capturing output, or everything stopped: the
    /// window manager hears.
    pub fn wm_capture(&self, output: &Output, started: bool) {
        for (o, resource) in &self.wm.outputs {
            if o == output {
                if started {
                    resource.capture_started();
                } else {
                    resource.capture_stopped();
                }
            }
        }
    }

    /// A window is gone: the window manager hears so, and should destroy it.
    pub fn wm_window_closed(&mut self, id: WindowId) {
        if let Some(resource) = self.wm.windows.remove(&id) {
            resource.closed();
        }
    }
}

/// An output's name, geometry and usable area, then done.
fn send_output_state(resource: &WayluckWmOutputV1, output: &Output, space: &Space<Window>) {
    resource.name(output.name());
    if let Some(geo) = space.output_geometry(output) {
        resource.geometry(geo.loc.x, geo.loc.y, geo.size.w, geo.size.h);
        // What bars leave windows (layers.rs), in global coordinates.
        let mut usable = layer_map_for_output(output).non_exclusive_zone();
        usable.loc += geo.loc;
        resource.usable(usable.loc.x, usable.loc.y, usable.size.w, usable.size.h);
    }
    resource.done();
}

impl Dispatch<WayluckWmOutputV1, Output> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WayluckWmOutputV1,
        _request: wayluck_wm_output_v1::Request,
        _data: &Output,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // Only destroy, handled below.
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        resource: &WayluckWmOutputV1,
        output: &Output,
    ) {
        state.wm.outputs.retain(|(_, r)| r != resource);
        // A virtual output goes with it (a monitor stays).
        state.remove_virtual_output(output);
    }
}

impl Dispatch<WayluckWmWindowV1, WindowId> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &WayluckWmWindowV1,
        request: wayluck_wm_window_v1::Request,
        id: &WindowId,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            // Asking the client: it may close, or not.
            wayluck_wm_window_v1::Request::Close => {
                if let Some(t) = state.windows.get(*id).and_then(|w| w.toplevel()) {
                    t.send_close();
                }
            }
            wayluck_wm_window_v1::Request::Destroy => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        _resource: &WayluckWmWindowV1,
        id: &WindowId,
    ) {
        state.wm.windows.remove(id);
    }
}
