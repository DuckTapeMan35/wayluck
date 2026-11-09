//! xdg-activation: a Wayland client asking for a window to be brought
//! forward. With a token from input the user just gave it, the window
//! manager hears it asked, and decides; without, the window only wants
//! the user's attention, until it's focused.

use crate::{Wayluck, windows::Windows};
use smithay::{
    input::Seat,
    reexports::wayland_server::{DisplayHandle, protocol::wl_surface::WlSurface},
    wayland::xdg_activation::{
        XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
    },
};
use std::time::Duration;

/// How long a token is good for, once made.
const FRESH: Duration = Duration::from_secs(10);

impl XdgActivationHandler for Wayluck {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.activation_state
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // Good once.
        self.activation_state.remove_token(&token);
        let Some(id) = Windows::id_of(&surface) else {
            return;
        };
        if self.made_from_input(&data) {
            self.wm_activate_request(id);
        } else {
            self.windows.asked_attention.insert(id);
            self.wm_window_attention(id);
        }
    }
}

impl Wayluck {
    pub fn activation_global(dh: &DisplayHandle) -> XdgActivationState {
        XdgActivationState::new::<Self>(dh)
    }

    /// Whether a token was made from input the user gave a client that
    /// still has the keyboard (its serial no older than the keyboard's
    /// last going there), not long ago.
    fn made_from_input(&self, data: &XdgActivationTokenData) -> bool {
        let Some((serial, seat)) = &data.serial else {
            return false;
        };
        let ours = Seat::<Wayluck>::from_resource(seat).is_some_and(|s| s == self.seat);
        let enter = self.seat.get_keyboard().and_then(|k| k.last_enter());
        let since_focus = enter.is_some_and(|enter| serial.is_no_older_than(&enter));
        ours && since_focus && data.timestamp.elapsed() < FRESH
    }
}
