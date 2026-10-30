//! Running on the hardware, from a TTY, as the session. libseat gives
//! wayluck the GPU and the input devices (and takes them back while you're
//! on another TTY); libinput reads the keyboards and mice; the first
//! connected monitor shows what's drawn, a frame per vblank at most.
//!
//! Ctrl+Alt+F1…F12 switch TTYs, and Ctrl+Alt+Backspace exits (input.rs).

use crate::{
    Wayluck,
    image_capture::{self, DmabufTarget},
    render, screencopy,
};
use smithay::{
    backend::{
        allocator::{
            Fourcc,
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
        udev::{all_gpus, primary_gpu},
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
use std::{cell::RefCell, error::Error, rc::Rc};

type Gpus = GpuManager<GbmGlesBackend<GlesRenderer, DrmDeviceFd>>;
type Compositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

/// The backend's own state, shared by its event sources.
struct Udev {
    /// Kept for as long as wayluck has the session: dropping it gives the
    /// TTY back.
    _session: LibSeatSession,
    /// The keyboards and mice: let go of with the GPU, taken back with it.
    libinput: Libinput,
    node: DrmNode,
    gpus: Gpus,
    drm: DrmDevice,
    compositor: Compositor,
    output: Output,
    /// A frame is on its way to the screen: the next waits for its vblank.
    in_flight: bool,
    /// Something changed while one was: draw again at the vblank.
    again: bool,
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
    let (mut drm, drm_events) = DrmDevice::new(fd.clone(), true)?;
    // Explicit sync, if the GPU's driver can wait on its timelines.
    if supports_syncobj_eventfd(&fd) {
        let syncobj = DrmSyncobjState::new::<Wayluck>(&state.display_handle, fd.clone());
        state.syncobj_state = Some(syncobj);
        tracing::info!("explicit sync (linux-drm-syncobj)");
    }
    let gbm = GbmDevice::new(fd.clone())?;
    let mut gpus: Gpus = GpuManager::new(GbmGlesBackend::default())?;
    gpus.as_mut().add_node(node, gbm.clone())?;

    // The first connected monitor, at the mode it prefers.
    let (monitor, mode, crtc) = first_monitor(&fd)?;
    let name = format!(
        "{}-{}",
        monitor.interface().as_str(),
        monitor.interface_id()
    );
    let (w, h) = monitor.size().unwrap_or((0, 0));
    let output = Output::new(
        name.clone(),
        PhysicalProperties {
            size: (w as i32, h as i32).into(),
            subpixel: Subpixel::Unknown,
            make: "unknown".into(),
            model: "unknown".into(),
        },
    );
    let _global = output.create_global::<Wayluck>(&state.display_handle);
    let wl_mode = Mode::from(mode);
    output.change_current_state(
        Some(wl_mode),
        Some(Transform::Normal),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(wl_mode);
    state.space.map_output(&output, (0, 0));
    tracing::info!(
        "{name}: {}x{} at {} mHz",
        wl_mode.size.w,
        wl_mode.size.h,
        wl_mode.refresh
    );

    // What puts frames on it: buffers the GPU renders into and the monitor
    // can show, and smithay's DrmCompositor, which tracks damage and puts
    // buffers straight on display planes when it can.
    let surface = drm.create_surface(crtc, mode, &[monitor.handle()])?;
    let render_formats = {
        let mut renderer = gpus.single_renderer(&node)?;
        renderer
            .as_mut()
            .egl_context()
            .dmabuf_render_formats()
            .clone()
    };
    let compositor = DrmCompositor::new(
        OutputModeSource::Auto(output.clone()),
        surface,
        None,
        GbmAllocator::new(
            gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        ),
        GbmFramebufferExporter::new(gbm.clone(), Some(node)),
        [Fourcc::Xrgb8888, Fourcc::Argb8888],
        render_formats,
        drm.cursor_size(),
        Some(gbm),
    )?;

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

    // Ctrl+Alt+F-keys (input.rs) ask the session for another TTY.
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
        compositor,
        output,
        in_flight: false,
        again: false,
    }));
    dmabuf(state, &udev)?;

    // Frames: drawn when something changed (queue_redraw), one at a time.
    let (ping, asked) = make_ping()?;
    let u = udev.clone();
    event_loop
        .handle()
        .insert_source(asked, move |_, _, state| draw_or_wait(state, &u))?;
    state.redraw = Some(ping);
    state.queue_redraw();

    // A vblank: the frame queued is on screen. Clients may draw again, and
    // a change that waited for it is drawn now.
    let u = udev.clone();
    event_loop
        .handle()
        .insert_source(drm_events, move |event, _, state| match event {
            DrmEvent::VBlank(_) => {
                let again = {
                    let mut udev = u.borrow_mut();
                    if let Err(e) = udev.compositor.frame_submitted() {
                        tracing::warn!("a frame shown: {e}");
                    }
                    udev.in_flight = false;
                    std::mem::take(&mut udev.again)
                };
                let output = u.borrow().output.clone();
                render::frame_done(state, &output);
                if again {
                    draw_or_wait(state, &u);
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
                if let Err(e) = udev.compositor.reset_state() {
                    tracing::warn!("resetting the monitor: {e}");
                }
                udev.in_flight = false;
                drop(udev);
                state.queue_redraw();
            }
        })?;
    Ok(())
}

/// The first connected monitor, its preferred mode (or else its first), and
/// a CRTC that can drive it.
fn first_monitor(
    fd: &DrmDeviceFd,
) -> Result<
    (
        connector::Info,
        smithay::reexports::drm::control::Mode,
        crtc::Handle,
    ),
    Box<dyn Error>,
> {
    let resources = fd.resource_handles()?;
    for &handle in resources.connectors() {
        let info = fd.get_connector(handle, false)?;
        if info.state() != connector::State::Connected {
            continue;
        }
        let modes = info.modes();
        let Some(&mode) = modes
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or(modes.first())
        else {
            continue;
        };
        for &encoder in info.encoders() {
            let encoder = fd.get_encoder(encoder)?;
            if let Some(&crtc) = resources.filter_crtcs(encoder.possible_crtcs()).first() {
                return Ok((info, mode, crtc));
            }
        }
    }
    Err("no monitor connected".into())
}

/// Draw a frame now, or after the one on its way (a vblank from now).
fn draw_or_wait(state: &mut Wayluck, udev: &Rc<RefCell<Udev>>) {
    let mut u = udev.borrow_mut();
    if u.in_flight {
        u.again = true;
        return;
    }
    let Udev {
        node,
        gpus,
        compositor,
        output,
        ..
    } = &mut *u;
    let mut renderer = match gpus.single_renderer(node) {
        Ok(r) => r,
        Err(e) => return tracing::warn!("no renderer: {e}"),
    };
    let elements = render::elements(state, output, &mut renderer);
    let rendered =
        compositor.render_frame(&mut renderer, &elements, render::CLEAR, FrameFlags::DEFAULT);
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
    // Screenshots and screen sharing waiting for this frame.
    screencopy::capture_virtual(state, &mut renderer);
    image_capture::capture_frames(state, &mut renderer);
    let output = output.clone();
    u.in_flight = shown;
    drop(u);
    // Nothing new to show: clients waiting to draw needn't wait for a vblank.
    if !shown {
        render::frame_done(state, &output);
    }
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
