//! Monitors: outputs with a screen behind them. They sit side by side, left
//! to right in the order they came, tops lined up; virtual outputs sit right
//! of them all, where the pointer never goes. Backends hand wayluck the
//! monitors they find and say when one goes; clients and the window manager hear of each.

pub mod management;

use crate::{Wayluck, capture::image_capture};
use smithay::{
    backend::input::InputTime,
    desktop::layer_map_for_output,
    output::{Mode, Output},
    reexports::wayland_server::backend::GlobalId,
    utils::{Logical, Point, Size, Transform},
};

/// A monitor, and the wl_output global clients bind it by.
pub struct Monitor {
    pub output: Output,
    global: Option<GlobalId>,
    pub placed: Option<Point<i32, Logical>>,
}

impl Monitor {
    pub fn enabled(&self) -> bool {
        self.global.is_some()
    }
}

impl Wayluck {
    /// A monitor came, showing mode (turned by transform): right of the
    /// others. Clients and the window manager hear of it.
    pub fn add_monitor(&mut self, output: Output, mode: Mode, transform: Transform) {
        output.change_current_state(Some(mode), Some(transform), None, None);
        output.set_preferred(mode);
        let global = output.create_global::<Wayluck>(&self.display_handle);
        tracing::info!(
            "{}: {}x{} at {} mHz",
            output.name(),
            mode.size.w,
            mode.size.h,
            mode.refresh
        );
        self.monitors.push(Monitor {
            output: output.clone(),
            placed: None,
            global: Some(global),
        });
        self.monitor_shown(&output);
    }

    fn monitor_shown(&mut self, output: &Output) {
        self.lay_out();
        self.wm_output_added(output);
        self.usable_changed(output);
        self.workspace_list_changed();
        self.queue_redraw();
    }

    pub fn remove_monitor(&mut self, output: &Output) {
        let Some(at) = self.monitors.iter().position(|m| &m.output == output) else {
            return;
        };
        let gone = self.monitors.remove(at);
        match gone.global {
            Some(global) => {
                self.display_handle.remove_global::<Wayluck>(global);
                self.monitor_hidden(output);
            }
            // Disabled, it was shown nowhere already.
            None => self.output_heads_changed(),
        }
        tracing::info!("{}: gone", output.name());
    }

    pub fn monitor_hidden(&mut self, output: &Output) {
        let layers: Vec<_> = layer_map_for_output(output).layers().cloned().collect();
        for layer in layers {
            layer_map_for_output(output).unmap_layer(&layer);
            layer.layer_surface().send_close();
            self.layer_gone(&layer);
        }
        self.scenes.show(output, None);
        self.space.unmap_output(output);
        self.fail_screencopies(output);
        self.stop_capture_sessions(&image_capture::Source::Output(output.clone()));
        self.wm_output_removed(output);
        self.lay_out();
        // The pointer, if it was there: onto the nearest monitor left.
        let pointer = self.seat.get_pointer().unwrap().current_location();
        let kept = self.keep_on_monitors(pointer, pointer);
        if kept != pointer {
            self.move_pointer(kept, InputTime::now());
        }
        if self.wm.manager.is_none() {
            self.show_own_scene();
        }
        self.sync_space();
        self.workspace_list_changed();
        self.taskbars_changed();
        self.x_work_area_changed();
        tracing::info!("{}: gone", output.name());
    }

    /// A monitor shown, or not (disabled), as a tool asks. Disabled, it's
    /// as if it went, but for output management, which still has it.
    pub fn enable_monitor(&mut self, output: &Output, enabled: bool) {
        let Some(monitor) = self.monitors.iter_mut().find(|m| m.output == *output) else {
            return;
        };
        match (monitor.global.take(), enabled) {
            (None, true) => {
                monitor.global = Some(output.create_global::<Wayluck>(&self.display_handle));
                tracing::info!("{}: enabled", output.name());
                self.monitor_shown(output);
            }
            (Some(global), false) => {
                self.display_handle.remove_global::<Wayluck>(global);
                if let Some(blank) = &self.blank_monitor {
                    blank(output);
                }
                tracing::info!("{}: disabled", output.name());
                self.monitor_hidden(output);
            }
            (global, _) => monitor.global = global,
        }
    }

    /// Every output where it goes: monitors put somewhere there, the others
    /// side by side right of them, then virtual outputs.
    pub fn lay_out(&mut self) {
        let width = |o: &Output| size(o).w;
        let shown = || self.monitors.iter().filter(|m| m.enabled());
        let mut x = shown()
            .filter_map(|m| Some(m.placed?.x + width(&m.output)))
            .max()
            .unwrap_or(0);
        let mut places = Vec::new();
        for monitor in shown() {
            let at = monitor.placed.unwrap_or_else(|| {
                let at = (x, 0).into();
                x += width(&monitor.output);
                at
            });
            places.push((monitor.output.clone(), at));
        }
        for v in &self.virtuals {
            places.push((v.output.clone(), (x, 0).into()));
            x += width(&v.output);
        }
        for (output, at) in places {
            if self.space.output_geometry(&output).map(|g| g.loc) != Some(at) {
                output.change_current_state(None, None, None, Some(at));
                self.space.map_output(&output, at);
                self.wm_output_changed(&output);
            }
        }
        self.output_heads_changed();
    }

    /// The monitor at location, if one is there.
    pub fn monitor_at(&self, location: Point<f64, Logical>) -> Option<&Output> {
        self.monitors().find(|o| {
            (self.space.output_geometry(o)).is_some_and(|g| g.to_f64().contains(location))
        })
    }

    /// Where the pointer goes, asked to go to location from from: there, if
    /// that's on a monitor; else as near as it gets on the one it's on.
    pub fn keep_on_monitors(
        &self,
        from: Point<f64, Logical>,
        location: Point<f64, Logical>,
    ) -> Point<f64, Logical> {
        if self.monitor_at(location).is_some() {
            return location;
        }
        let on = self.monitor_at(from).or(self.monitors().next());
        let Some(area) = on.and_then(|o| self.space.output_geometry(o)) else {
            return location;
        };
        let (x0, y0) = (area.loc.x as f64, area.loc.y as f64);
        (
            location.x.clamp(x0, x0 + area.size.w as f64 - 1.0),
            location.y.clamp(y0, y0 + area.size.h as f64 - 1.0),
        )
            .into()
    }
}

/// An output's size, where it's mapped (as smithay's space has it).
pub fn size(output: &Output) -> Size<i32, Logical> {
    let mode = output.current_mode().map_or_else(Size::default, |m| m.size);
    let scale = output.current_scale().fractional_scale();
    let turned = output.current_transform().transform_size(mode);
    turned.to_f64().to_logical(scale).to_i32_ceil()
}
