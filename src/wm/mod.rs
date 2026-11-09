//! The window manager's side of wayluck: the wayluck_wm_manager_v1 global.
//! One window manager at a time; whoever binds while one is bound gets
//! `unavailable`.

mod bindings;
mod outputs;
pub mod protocol;
pub mod scenes;
mod transaction;
pub mod virtual_output;
mod windows;

pub use bindings::modifier_bits;

use crate::{
    Wayluck,
    render::blur::Strength,
    windows::{Curve, Setting, WindowId, Windows},
};
use bindings::{ButtonBinding, KeyBinding};
use protocol::{
    wayluck_wm_button_binding_v1::WayluckWmButtonBindingV1,
    wayluck_wm_key_binding_v1::WayluckWmKeyBindingV1,
    wayluck_wm_manager_v1::{self, WayluckWmManagerV1},
    wayluck_wm_output_v1::WayluckWmOutputV1,
    wayluck_wm_scene_v1::WayluckWmSceneV1,
    wayluck_wm_window_v1::WayluckWmWindowV1,
};
use scenes::SceneId;
use smithay::{
    backend::input::InputTime,
    output::{Output, PhysicalProperties, Subpixel},
    reexports::wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, backend::ClientId,
    },
};
use std::{cell::RefCell, collections::HashMap, time::Duration};
use transaction::Transaction;

const VERSION: u32 = 1;

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
    /// Windows it's been told about since the last done: each gets one,
    /// once the loop has handled all there was.
    told: RefCell<Vec<WindowId>>,
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
                        serial_number: String::new(),
                    };
                    data_init.init(id, Output::new("NONE".into(), nowhere));
                    return;
                }
                let template = template.and_then(|t| t.data::<Output>().cloned());
                let size = (width, height).into();
                let output = state.add_virtual_output(size, template, refresh);
                let resource = data_init.init(id, output.clone());
                outputs::send_output_state(&resource, &output, state);
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
            wayluck_wm_manager_v1::Request::OwnWindow { id, surface } => {
                // A toplevel of its own client's, that isn't announced yet.
                let window = Windows::id_of(&surface).filter(|w| {
                    surface.client().as_ref() == manager.client().as_ref()
                        && !state.wm.windows.contains_key(w)
                });
                let Some(window) = window else {
                    manager.post_error(
                        wayluck_wm_manager_v1::Error::NotOwnToplevel,
                        "own_window: not a toplevel of the window manager's own",
                    );
                    return;
                };
                let resource = data_init.init(id, window);
                state.windows.own(window);
                // Known now: at its first commit, it isn't announced.
                state.wm.windows.insert(window, resource);
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
                    state.move_pointer((x as f64, y as f64).into(), InputTime::now());
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
            wayluck_wm_manager_v1::Request::SetAnimation { duration, curve } => {
                let Some(setting) = animation(manager, duration, &curve) else {
                    return;
                };
                if state.wm.manager.as_ref() == Some(manager) {
                    state.windows.set_animation(None, Some(setting));
                }
            }
            wayluck_wm_manager_v1::Request::SetBlur { passes, offset } => {
                let Some(strength) = Strength::new(passes, offset as f32) else {
                    manager.post_error(
                        wayluck_wm_manager_v1::Error::InvalidBlur,
                        "set_blur: passes aren't 1 to 8, or the offset isn't above 0",
                    );
                    return;
                };
                if state.wm.manager.as_ref() == Some(manager) {
                    state.windows.blur = strength;
                }
            }
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, manager: &WayluckWmManagerV1, _data: &()) {
        if state.wm.manager.as_ref() == Some(manager) {
            tracing::info!("window manager gone");
            // Its objects go with it; windows stay where they are, in wayluck's own scene.
            state.wm = WmState::default();
            // How it had windows move goes with it.
            state.windows.forget_animations();
            state.configure_waiting();
            state.show_pending();
            state.show_own_scene();
        }
    }
}

/// How windows move, as the window manager asks (duration in
/// milliseconds, the curve as an array of 32-bit floats); None if the curve
/// isn't one: manager's error.
pub(super) fn animation(
    manager: &WayluckWmManagerV1,
    duration: u32,
    curve: &[u8],
) -> Option<Setting> {
    let samples = curve
        .chunks_exact(4)
        .map(|f| f32::from_ne_bytes([f[0], f[1], f[2], f[3]]));
    let curve = Curve::new(samples.collect()).filter(|_| curve.len().is_multiple_of(4));
    let Some(curve) = curve else {
        manager.post_error(
            wayluck_wm_manager_v1::Error::InvalidCurve,
            "set_animation: the curve isn't 2 to 1024 floats, all numbers",
        );
        return None;
    };
    let duration = Duration::from_millis(duration.into());
    Some(Setting { duration, curve })
}

/// The window a window manager's object stands for.
pub(super) fn id_of(window: &WayluckWmWindowV1) -> WindowId {
    *window
        .data::<WindowId>()
        .expect("window objects are made with their id")
}

pub(super) fn scene_of(scene: &WayluckWmSceneV1) -> SceneId {
    *scene.data::<SceneId>().unwrap()
}

impl Wayluck {
    /// A window manager was just accepted: every output, every window, then ready.
    fn wm_announce_all(&mut self) {
        let outputs: Vec<Output> = self.monitors().cloned().collect();
        for output in outputs {
            self.wm_output_added(&output);
        }
        // Windows that have committed (and X windows); the rest are
        // announced at their first commit.
        // Nothing is shown until it says what (its first transaction).
        self.scenes.show_nothing();
        self.sync_space();
        let ids: Vec<WindowId> = self
            .windows
            .ids()
            .filter(|&id| {
                // An X window is one once it's shown.
                (self.windows.get(id))
                    .is_some_and(|w| w.toplevel().is_none_or(|t| t.is_initial_configure_sent()))
            })
            .collect();
        for id in ids {
            self.wm_window_added(id);
        }
        if let Some(m) = &self.wm.manager {
            m.ready();
        }
    }
}
