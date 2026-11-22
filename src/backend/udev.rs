//! Running on the hardware, from a TTY, as the session. libseat gives
//! wayluck the GPU and the input devices (and takes them back while you're
//! on another TTY); libinput reads the keyboards and mice; every connected
//! monitor shows what's drawn, a frame per vblank at most, each driven by
//! a CRTC of its own. Monitors plugged in or out are found again (udev
//! says the GPU changed).
//!
//! Ctrl+Alt+F1…F12 switch TTYs, and Ctrl+Alt+Backspace exits.

use crate::{
    Wayluck,
    capture::image_capture::{self, DmabufTarget},
    capture::screencopy,
    render,
};
use smithay::{
    backend::{
        allocator::{
            Format, Fourcc,
            dmabuf::Dmabuf,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType,
            compositor::{DrmCompositor, FrameFlags},
            exporter::gbm::GbmFramebufferExporter,
        },
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            Bind, ImportDma,
            gles::GlesRenderer,
            multigpu::{GpuManager, gbm::GbmGlesBackend},
        },
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{UdevBackend, UdevEvent, all_gpus, primary_gpu},
    },
    output::{Mode, Output, OutputModeSource, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{EventLoop, ping::make_ping},
        drm::control::{Device as _, ModeTypeFlags, connector, crtc},
        input::Libinput,
        rustix::fs::OFlags,
    },
    utils::{DeviceFd, Transform},
    wayland::{
        dmabuf::DmabufFeedbackBuilder,
        drm_syncobj::{DrmSyncobjState, supports_syncobj_eventfd},
    },
};
use std::{cell::RefCell, collections::HashSet, error::Error, rc::Rc};

type Gpus = GpuManager<GbmGlesBackend<GlesRenderer, DrmDeviceFd>>;
type Compositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

/// The backend's own state, shared by its event sources.
struct Udev {
    /// Kept for as long as wayluck has the session: dropping it gives the TTY back.
    _session: LibSeatSession,
    /// The keyboards and mice: let go of with the GPU, taken back with it.
    libinput: Libinput,
    node: DrmNode,
    gpus: Gpus,
    drm: DrmDevice,
    fd: DrmDeviceFd,
    gbm: GbmDevice<DrmDeviceFd>,
    /// What it can render into.
    render_formats: Vec<Format>,
    /// The monitors it drives.
    screens: Vec<Screen>,
}

/// A monitor shown: its connector, the CRTC driving it, and what puts
/// frames on it (smithay's DrmCompositor, which tracks damage and puts
/// buffers straight on display planes when it can).
struct Screen {
    connector: connector::Handle,
    crtc: crtc::Handle,
    output: Output,
    compositor: Compositor,
    /// A frame is on its way to the screen: the next waits for its vblank.
    in_flight: bool,
    /// Something changed while one was: draw again at the vblank.
    again: bool,
    /// The modes the monitor can show.
    modes: Vec<smithay::reexports::drm::control::Mode>,
}

pub fn init(
    event_loop: &mut EventLoop<'static, Wayluck>,
    state: &mut Wayluck,
) -> Result<(), Box<dyn Error>> {
    // The session: logind or seatd hands over the devices of this seat.
    let (mut session, session_events) = LibSeatSession::new()?;
    let seat = session.seat();

    // The GPU: the one that drives the boot display, or else the first.
    let path = primary_gpu(&seat)?
        .or_else(|| all_gpus(&seat).ok()?.into_iter().next())
        .ok_or("no GPU on this seat")?;
    let node = DrmNode::from_path(&path)?
        .node_with_type(NodeType::Render)
        .and_then(Result::ok)
        .unwrap_or(DrmNode::from_path(&path)?);
    tracing::info!("GPU {} (renders on {node})", path.display());
    let fd = session.open(
        &path,
        OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
    )?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let (drm, drm_events) = DrmDevice::new(fd.clone(), true)?;
    // Explicit sync, if the GPU's driver can wait on its timelines.
    if supports_syncobj_eventfd(&fd) {
        let syncobj = DrmSyncobjState::new::<Wayluck>(&state.display_handle, fd.clone());
        state.syncobj_state = Some(syncobj);
        tracing::info!("explicit sync (linux-drm-syncobj)");
    }
    let gbm = GbmDevice::new(fd.clone())?;
    let mut gpus: Gpus = GpuManager::new(GbmGlesBackend::default())?;
    gpus.as_mut().add_node(node, gbm.clone())?;
    let render_formats = {
        let mut renderer = gpus.single_renderer(&node)?;
        let context = renderer.as_mut().egl_context();
        context.dmabuf_render_formats().iter().copied().collect()
    };

    // Input: libinput opens the seat's keyboards and mice through the
    // session, as the GPU was.
    let mut libinput = Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
    libinput
        .udev_assign_seat(&seat)
        .map_err(|()| "libinput couldn't take this seat's devices")?;
    event_loop.handle().insert_source(
        LibinputInputBackend::new(libinput.clone()),
        |event, _, state| {
            if let InputEvent::DeviceAdded { device } = &event {
                tracing::info!("input: {}", device.name());
            }
            state.handle_input(event);
        },
    )?;

    // Ctrl+Alt+F-keys ask the session for another TTY.
    let vt = session.clone();
    state.change_vt = Some(Box::new(move |n| {
        if let Err(e) = vt.clone().change_vt(n) {
            tracing::warn!("switching to TTY {n}: {e}");
        }
    }));
    // There's no host to draw the pointer: wayluck does.
    state.cursor.drawn = true;

    let udev = Rc::new(RefCell::new(Udev {
        _session: session,
        libinput,
        node,
        gpus,
        drm,
        fd,
        gbm,
        render_formats,
        screens: Vec::new(),
    }));
    dmabuf(state, &udev)?;

    let u = udev.clone();
    state.blank_monitor = Some(Box::new(move |output| {
        let mut udev = u.borrow_mut();
        let Some(screen) = udev.screens.iter_mut().find(|s| s.output == *output) else {
            return;
        };
        if let Err(e) = screen.compositor.clear() {
            tracing::warn!("{}: blanking: {e}", output.name());
        }
        screen.in_flight = false;
        screen.again = false;
    }));

    // Another mode for a monitor: shown from its next frame.
    let u = udev.clone();
    state.set_mode = Some(Box::new(move |output, mode| {
        let mut udev = u.borrow_mut();
        let Some(screen) = udev.screens.iter_mut().find(|s| s.output == *output) else {
            return false;
        };
        let Some(&drm) = screen.modes.iter().find(|&&m| Mode::from(m) == mode) else {
            return false;
        };
        match screen.compositor.use_mode(drm) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("{}: another mode: {e}", output.name());
                false
            }
        }
    }));
    // The monitors connected now; then those plugged in or out, as udev
    // says the GPU changed.
    scan(state, &udev);
    let card = DrmNode::from_path(&path)?.dev_id();
    let u = udev.clone();
    event_loop
        .handle()
        .insert_source(UdevBackend::new(&seat)?, move |event, _, state| {
            if let UdevEvent::Changed { device_id } = event
                && device_id == card
            {
                scan(state, &u);
            }
        })?;

    // Frames: drawn when something changed (queue_redraw), one at a time.
    let (ping, asked) = make_ping()?;
    let u = udev.clone();
    event_loop
        .handle()
        .insert_source(asked, move |_, _, state| draw_all(state, &u))?;
    state.redraw = Some(ping);
    state.queue_redraw();

    // A vblank: the frame queued is on screen. Clients may draw again, and
    // a change that waited for it is drawn now.
    let u = udev.clone();
    event_loop
        .handle()
        .insert_source(drm_events, move |event, _, state| match event {
            DrmEvent::VBlank(crtc) => {
                let shown = {
                    let mut udev = u.borrow_mut();
                    let screen = udev.screens.iter_mut().find(|s| s.crtc == crtc);
                    screen.map(|screen| {
                        if let Err(e) = screen.compositor.frame_submitted() {
                            tracing::warn!("a frame shown: {e}");
                        }
                        screen.in_flight = false;
                        (screen.output.clone(), std::mem::take(&mut screen.again))
                    })
                };
                if let Some((output, again)) = shown {
                    render::frame_done(state, &output);
                    if again {
                        draw_or_wait(state, &u, &output);
                        finish_frame(state, &u);
                    }
                }
            }
            DrmEvent::Error(e) => tracing::warn!("DRM: {e}"),
        })?;

    // Another TTY: the GPU isn't ours meanwhile. Back: draw it all again.
    let u = udev.clone();
    event_loop
        .handle()
        .insert_source(session_events, move |event, _, state| match event {
            SessionEvent::PauseSession => {
                tracing::info!("session paused");
                let mut udev = u.borrow_mut();
                udev.libinput.suspend();
                udev.drm.pause();
            }
            SessionEvent::ActivateSession => {
                tracing::info!("session back");
                let mut udev = u.borrow_mut();
                if let Err(e) = udev.drm.activate(false) {
                    tracing::warn!("taking the GPU back: {e}");
                }
                if udev.libinput.resume().is_err() {
                    tracing::warn!("taking the input devices back");
                }
                for screen in &mut udev.screens {
                    if let Err(e) = screen.compositor.reset_state() {
                        tracing::warn!("resetting {}: {e}", screen.output.name());
                    }
                    screen.in_flight = false;
                }
                drop(udev);
                // Monitors may have come or gone meanwhile.
                scan(state, &u);
                state.queue_redraw();
            }
        })?;
    Ok(())
}

/// The monitors as they are now: one connected and not shown yet is
/// shown, at the mode it prefers, on a CRTC no other uses; one shown and
/// no longer connected goes.
fn scan(state: &mut Wayluck, udev: &Rc<RefCell<Udev>>) {
    let connected = match connected(&udev.borrow().fd) {
        Ok(connected) => connected,
        Err(e) => return tracing::warn!("listing monitors: {e}"),
    };
    let gone: Vec<Output> = {
        let mut u = udev.borrow_mut();
        let (stay, gone): (Vec<_>, Vec<_>) =
            std::mem::take(&mut u.screens).into_iter().partition(|s| {
                connected
                    .iter()
                    .any(|(info, _)| info.handle() == s.connector)
            });
        u.screens = stay;
        gone.into_iter().map(|s| s.output).collect()
    };
    for output in gone {
        state.remove_monitor(&output);
    }
    for (info, mode) in connected {
        if udev
            .borrow()
            .screens
            .iter()
            .any(|s| s.connector == info.handle())
        {
            continue;
        }
        match show(state, udev, &info, mode) {
            Ok(()) => {}
            Err(e) => tracing::warn!("can't show {:?}: {e}", info.interface()),
        }
    }
}

/// The connectors with a monitor on them, and the mode each prefers (or
/// else its first).
fn connected(
    fd: &DrmDeviceFd,
) -> Result<Vec<(connector::Info, smithay::reexports::drm::control::Mode)>, Box<dyn Error>> {
    let mut connected = Vec::new();
    for &handle in fd.resource_handles()?.connectors() {
        let info = fd.get_connector(handle, false)?;
        if info.state() != connector::State::Connected {
            continue;
        }
        let modes = info.modes();
        let preferred = modes
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED));
        if let Some(&mode) = preferred.or(modes.first()) {
            connected.push((info, mode));
        }
    }
    Ok(connected)
}

/// Show the monitor on a connector: a CRTC no other monitor uses, buffers
/// the GPU renders into and the monitor can show, and an output for it.
fn show(
    state: &mut Wayluck,
    udev: &Rc<RefCell<Udev>>,
    info: &connector::Info,
    mode: smithay::reexports::drm::control::Mode,
) -> Result<(), Box<dyn Error>> {
    let mut u = udev.borrow_mut();
    let used: HashSet<crtc::Handle> = u.screens.iter().map(|s| s.crtc).collect();
    let resources = u.fd.resource_handles()?;
    let mut free = None;
    for &encoder in info.encoders() {
        let encoder = u.fd.get_encoder(encoder)?;
        let crtcs = resources.filter_crtcs(encoder.possible_crtcs());
        if let Some(&crtc) = crtcs.iter().find(|c| !used.contains(c)) {
            free = Some(crtc);
            break;
        }
    }
    let crtc = free.ok_or("no CRTC left to drive it")?;
    let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
    let (w, h) = info.size().unwrap_or((0, 0));
    let output = Output::new(
        name,
        PhysicalProperties {
            size: (w as i32, h as i32).into(),
            subpixel: Subpixel::Unknown,
            make: "unknown".into(),
            model: "unknown".into(),
            serial_number: String::new(),
        },
    );
    let surface = u.drm.create_surface(crtc, mode, &[info.handle()])?;
    let gbm = u.gbm.clone();
    let compositor = DrmCompositor::new(
        OutputModeSource::Auto(output.downgrade()),
        surface,
        None,
        GbmAllocator::new(
            gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        ),
        GbmFramebufferExporter::new(gbm.clone(), u.node.into()),
        [Fourcc::Xrgb8888, Fourcc::Argb8888],
        u.render_formats.iter().copied(),
        u.drm.cursor_size(),
        Some(gbm),
    )?;
    for &mode in info.modes() {
        output.add_mode(Mode::from(mode));
    }
    u.screens.push(Screen {
        connector: info.handle(),
        crtc,
        output: output.clone(),
        compositor,
        in_flight: false,
        again: false,
        modes: info.modes().to_vec(),
    });
    drop(u);
    state.add_monitor(output, Mode::from(mode), Transform::Normal);
    Ok(())
}

/// Draw every monitor: now, or after its frame on its way.
fn draw_all(state: &mut Wayluck, udev: &Rc<RefCell<Udev>>) {
    let outputs: Vec<Output> = udev
        .borrow()
        .screens
        .iter()
        .map(|s| s.output.clone())
        .collect();
    for output in outputs {
        draw_or_wait(state, udev, &output);
    }
    finish_frame(state, udev);
}

/// Draw a frame on output now, or after the one on its way.
fn draw_or_wait(state: &mut Wayluck, udev: &Rc<RefCell<Udev>>, output: &Output) {
    let mut u = udev.borrow_mut();
    let Udev {
        node,
        gpus,
        screens,
        ..
    } = &mut *u;
    let Some(screen) = screens.iter_mut().find(|s| &s.output == output) else {
        return;
    };
    // Disabled
    if !state.monitors().any(|o| o == output) {
        return;
    }
    if screen.in_flight {
        screen.again = true;
        return;
    }
    let mut renderer = match gpus.single_renderer(node) {
        Ok(r) => r,
        Err(e) => return tracing::warn!("no renderer: {e}"),
    };
    let elements = render::elements(state, output, &mut renderer);
    let compositor = &mut screen.compositor;
    let rendered =
        compositor.render_frame(&mut renderer, &elements, render::CLEAR, FrameFlags::DEFAULT);
    let changed = matches!(&rendered, Ok(frame) if !frame.is_empty);
    tracing::debug!("drew a frame on {}: changed {changed}", output.name());
    let shown = match rendered {
        Ok(frame) if frame.is_empty => false,
        Ok(_) => match compositor.queue_frame(()) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("showing a frame: {e}");
                false
            }
        },
        Err(e) => {
            tracing::warn!("drawing a frame: {e}");
            false
        }
    };
    screen.in_flight = shown;
    drop(u);
    // Nothing new to show: clients waiting to draw needn't wait for a vblank.
    if !shown {
        render::frame_done(state, output);
    }
}

/// After drawing: screenshots and screen sharing waiting for a frame.
fn finish_frame(state: &mut Wayluck, udev: &Rc<RefCell<Udev>>) {
    let mut u = udev.borrow_mut();
    let Udev { node, gpus, .. } = &mut *u;
    let mut renderer = match gpus.single_renderer(node) {
        Ok(r) => r,
        Err(e) => return tracing::warn!("no renderer: {e}"),
    };
    screencopy::capture_virtual(state, &mut renderer);
    image_capture::capture_frames(state, &mut renderer);
}

/// linux-dmabuf v4 on this GPU: GPU clients render into buffers it can use
/// (and captures, into buffers it draws).
fn dmabuf(state: &mut Wayluck, udev: &Rc<RefCell<Udev>>) -> Result<(), Box<dyn Error>> {
    let (node, formats, render_formats) = {
        let mut u = udev.borrow_mut();
        let node = u.node;
        let renderer = u.gpus.single_renderer(&node)?;
        let formats = renderer.dmabuf_formats();
        let render_formats = Bind::<Dmabuf>::supported_formats(&renderer).unwrap_or_default();
        (node, formats, render_formats)
    };
    // Captures may be dmabufs on it too.
    state.image_capture.dmabuf = Some(DmabufTarget::new(node.dev_id(), &render_formats));
    let feedback = DmabufFeedbackBuilder::new(node.dev_id(), formats).build()?;
    let dh = state.display_handle.clone();
    state
        .dmabuf_state
        .create_global_with_default_feedback::<Wayluck>(&dh, &feedback);
    let u = udev.clone();
    state.import_dmabuf = Some(Box::new(move |dmabuf| {
        let mut u = u.borrow_mut();
        let node = u.node;
        u.gpus
            .single_renderer(&node)
            .is_ok_and(|mut r| r.import_dmabuf(dmabuf, None).is_ok())
    }));
    Ok(())
}
