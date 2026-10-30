//! Virtual outputs: outputs with no screen behind them, made by the window
//! manager to show a scene nobody looks at, only captures (screen sharing a
//! workspace). Clients see them as any wl_output. They sit in the space to
//! the right of everything, where the pointer never goes; layer surfaces
//! can't be put on them, but one may borrow its template output's (bars,
//! wallpapers, overlays) to look like it. Drawn only for captures.
//! While captured, its windows are paced by a timer at its
//! refresh rate (its template's, or its own).

use crate::{Wayluck, image_capture, render};
use smithay::{
    backend::renderer::damage::OutputDamageTracker,
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        wayland_server::backend::GlobalId,
    },
    utils::{Physical, Size, Transform},
};
use std::time::Duration;

pub struct VirtualOutput {
    pub output: Output,
    global: GlobalId,
    /// The output whose bars, wallpapers and overlays it draws.
    pub template: Option<Output>,
    /// What it last showed, to tell when something changed (screencopy's
    /// copy_with_damage).
    pub damage: OutputDamageTracker,
    /// The timer pacing its windows, while it's captured.
    pacer: Option<RegistrationToken>,
}

impl Wayluck {
    /// Make a virtual output: size pixels, modelled on template (or with no
    /// bars, paced at refresh mHz).
    pub fn add_virtual_output(
        &mut self,
        size: Size<i32, Physical>,
        template: Option<Output>,
        refresh: i32,
    ) -> Output {
        self.virtual_count += 1;
        let output = Output::new(
            format!("VIRTUAL-{}", self.virtual_count),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "wayluck".into(),
                model: "virtual".into(),
            },
        );
        let refresh = (template.as_ref())
            .and_then(Output::current_mode)
            .map_or(refresh, |m| m.refresh);
        let mode = Mode { size, refresh };
        // Right of every other output: the pointer, kept on the first, never
        // gets there.
        let x = (self.space.outputs())
            .filter_map(|o| self.space.output_geometry(o))
            .map(|g| g.loc.x + g.size.w)
            .max()
            .unwrap_or(0);
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            None,
            Some((x, 0).into()),
        );
        output.set_preferred(mode);
        let global = output.create_global::<Wayluck>(&self.display_handle);
        self.space.map_output(&output, (x, 0));
        tracing::info!("{}: {}x{}, virtual", output.name(), size.w, size.h);
        self.virtuals.push(VirtualOutput {
            damage: OutputDamageTracker::from_output(&output),
            output: output.clone(),
            global,
            template,
            pacer: None,
        });
        output
    }

    /// The window manager is done with a virtual output: it goes, and with
    /// it captures of it.
    pub fn remove_virtual_output(&mut self, output: &Output) {
        let Some(at) = self.virtuals.iter().position(|v| &v.output == output) else {
            return;
        };
        self.stop_pacing(output);
        self.captures.retain(|(o, _)| o != output);
        let gone = self.virtuals.remove(at);
        self.display_handle.remove_global::<Wayluck>(gone.global);
        self.scenes.show(output, None);
        self.space.unmap_output(output);
        self.fail_screencopies(output);
        self.stop_capture_sessions(&image_capture::Source::Output(output.clone()));
        self.sync_space();
        tracing::info!("{}: gone", output.name());
    }

    pub fn is_virtual(&self, output: &Output) -> bool {
        self.virtuals.iter().any(|v| &v.output == output)
    }

    /// Whose bars, wallpapers and overlays output draws: its own, its
    /// template's for a virtual output, none for one without.
    pub fn layers_from(&self, output: &Output) -> Option<Output> {
        match self.virtuals.iter().find(|v| &v.output == output) {
            Some(v) => v.template.clone(),
            None => Some(output.clone()),
        }
    }

    /// The outputs with a screen behind them.
    pub fn monitors(&self) -> impl Iterator<Item = &Output> {
        self.space.outputs().filter(|o| !self.is_virtual(o))
    }

    /// output started being captured: if it's virtual, its windows get frame
    /// callbacks at its refresh rate from now on (on a monitor, drawing it
    /// does that).
    pub fn start_pacing(&mut self, output: &Output) {
        let Some(at) = self.virtuals.iter().position(|v| &v.output == output) else {
            return;
        };
        let refresh = output.current_mode().map_or(0, |m| m.refresh);
        // mHz: 120000 is 120 Hz. None given: 60 Hz.
        let refresh = if refresh > 0 { refresh as u64 } else { 60_000 };
        let every = Duration::from_nanos(1_000_000_000_000 / refresh);
        let paced = output.clone();
        let pacer =
            self.loop_handle
                .insert_source(Timer::from_duration(every), move |_, _, state| {
                    render::frame_done(state, &paced);
                    TimeoutAction::ToDuration(every)
                });
        match pacer {
            Ok(token) => self.virtuals[at].pacer = Some(token),
            Err(e) => tracing::warn!("can't pace {}: {e}", output.name()),
        }
    }

    /// Nothing captures output any more: its windows aren't paced (nobody
    /// would see what they draw).
    pub fn stop_pacing(&mut self, output: &Output) {
        let pacer = (self.virtuals.iter_mut())
            .find(|v| &v.output == output)
            .and_then(|v| v.pacer.take());
        if let Some(token) = pacer {
            self.loop_handle.remove(token);
        }
    }
}
