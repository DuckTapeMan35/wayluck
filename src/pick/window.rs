//! The picker's window: a list to choose from, each with a live thumbnail,
//! drawn on the GPU (smithay's renderer, through winit, as a
//! Wayland client). Fixed size, so the window manager floats it. Up and Down
//! (or the pointer) select, Enter (or a click) chooses, Esc (or closing the
//! window) cancels.

use super::{
    capture::{Captures, Image},
    text::Text,
};
use smithay::{
    backend::{
        allocator::Fourcc,
        input::{
            AbsolutePositionEvent, ButtonState, InputEvent, KeyState, KeyboardKeyEvent,
            PointerButtonEvent,
        },
        renderer::{
            Color32F, Frame, ImportDma, ImportMem, Renderer, Texture,
            gles::{GlesError, GlesFrame, GlesRenderer, GlesTexture},
        },
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    reexports::{
        calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic},
        winit::{dpi::LogicalSize, platform::wayland::WindowAttributesExtWayland, window::Window},
    },
    utils::{Logical, Physical, Point, Rectangle, Size, Transform},
};
use std::os::fd::AsFd;
use wayland_client::{EventQueue, QueueHandle};

/// The window's app id: what the window manager knows it by.
const APP_ID: &str = "wayluck.picker";
const TITLE: &str = "Choose what to share";

// Its layout, in logical pixels: a heading, then a row for each choice.
const WIDTH: i32 = 480;
const PADDING: i32 = 12;
const HEADING: i32 = 44;
const ROW: i32 = 96;
const TEXT: f32 = 15.0;

/// The box a thumbnail fits in, keeping its shape: 16:9.
const THUMBNAIL: (i32, i32) = (128, 72);

const BACKGROUND: Color32F = Color32F::new(0.12, 0.12, 0.13, 1.0);
const SELECTED: Color32F = Color32F::new(0.23, 0.43, 0.65, 1.0);

/// Where a thumbnail will be, until its first frame.
const NO_THUMBNAIL: Color32F = Color32F::new(0.2, 0.2, 0.22, 1.0);

// Keys, as xkb keycodes (evdev's + 8): the same whatever the layout.
const KEY_ESC: u32 = 1 + 8;
const KEY_ENTER: u32 = 28 + 8;
const KEY_KP_ENTER: u32 = 96 + 8;
const KEY_UP: u32 = 103 + 8;
const KEY_DOWN: u32 = 108 + 8;
const BTN_LEFT: u32 = 0x110;

struct Picker {
    backend: WinitGraphicsBackend<GlesRenderer>,
    text: Text,
    /// What to choose from, as the user reads it.
    labels: Vec<String>,
    /// The heading and labels drawn, for the scale they were drawn at.
    drawn: Option<(f64, Vec<GlesTexture>)>,
    captures: Captures,
    /// To ask for the captures' next frames.
    qh: QueueHandle<Captures>,
    /// Each row's latest frame, once there is one.
    thumbnails: Vec<Option<GlesTexture>>,
    selected: usize,
    /// The row under the pointer, if any.
    hovered: Option<usize>,
    /// Some once the user decides: Some(row) chosen, None cancelled.
    answer: Option<Option<usize>>,
}

/// Show labels, one per row, with captures' thumbnails: the row the user
/// chooses, or None if they cancel.
pub fn choose(
    labels: Vec<String>,
    captures: Captures,
    mut queue: EventQueue<Captures>,
) -> Result<Option<usize>, Box<dyn std::error::Error>> {
    let text = Text::new()?;
    let height = HEADING + ROW * labels.len() as i32 + PADDING;
    let attributes = Window::default_attributes()
        .with_name(APP_ID, APP_ID)
        .with_title(TITLE)
        .with_inner_size(LogicalSize::new(WIDTH, height))
        .with_resizable(false)
        .with_decorations(false);
    let (backend, events) = winit::init_from_attributes::<GlesRenderer>(attributes)?;
    let qh = queue.handle();
    backend.window().request_redraw();

    let mut event_loop = EventLoop::<Picker>::try_new()?;
    event_loop
        .handle()
        .insert_source(events, |event, _, picker| picker.handle(event))?;
    // The captures' frames, when they come.
    let connection = captures.connection().as_fd().try_clone_to_owned()?;
    let readable = Generic::new(connection, Interest::READ, Mode::Level);
    event_loop
        .handle()
        .insert_source(readable, move |_, _, picker| {
            if let Some(reading) = queue.prepare_read() {
                // Nothing to read after all is no error.
                let _ = reading.read();
            }
            if let Err(e) = queue.dispatch_pending(&mut picker.captures) {
                // The thumbnails stay as they are; the picker still works.
                eprintln!("wayluck pick: thumbnails: {e}");
                return Ok(PostAction::Remove);
            }
            if std::mem::take(&mut picker.captures.fresh) {
                picker.backend.window().request_redraw();
            }
            Ok(PostAction::Continue)
        })?;
    let mut picker = Picker {
        backend,
        text,
        thumbnails: labels.iter().map(|_| None).collect(),
        labels,
        captures,
        qh,
        drawn: None,
        selected: 0,
        hovered: None,
        answer: None,
    };
    while picker.answer.is_none() {
        event_loop.dispatch(None, &mut picker)?;
    }
    Ok(picker.answer.flatten())
}

impl Picker {
    fn handle(&mut self, event: WinitEvent) {
        match event {
            WinitEvent::Redraw => {
                if let Err(e) = self.draw() {
                    eprintln!("wayluck pick: drawing: {e}");
                }
            }
            WinitEvent::Resized { .. } => self.backend.window().request_redraw(),
            WinitEvent::CloseRequested => self.answer = Some(None),
            WinitEvent::Input(InputEvent::Keyboard { event }) => {
                if event.state() == KeyState::Pressed {
                    self.key(event.key_code().raw());
                }
            }
            WinitEvent::Input(InputEvent::PointerMotionAbsolute { event }) => {
                let size = self.size().to_f64().to_logical(self.scale());
                let at = event.position_transformed(size.to_i32_round());
                self.hovered = self.row_at(at);
                if let Some(row) = self.hovered {
                    self.select(row);
                }
            }
            WinitEvent::Input(InputEvent::PointerButton { event }) => {
                if event.button_code() == BTN_LEFT
                    && event.state() == ButtonState::Pressed
                    && let Some(row) = self.hovered
                {
                    self.answer = Some(Some(row));
                }
            }
            _ => {}
        }
    }

    fn key(&mut self, key: u32) {
        let last = self.labels.len() - 1;
        match key {
            KEY_ESC => self.answer = Some(None),
            KEY_ENTER | KEY_KP_ENTER => self.answer = Some(Some(self.selected)),
            KEY_UP => self.select(self.selected.saturating_sub(1)),
            KEY_DOWN => self.select((self.selected + 1).min(last)),
            _ => {}
        }
    }

    fn select(&mut self, row: usize) {
        if row != self.selected {
            self.selected = row;
            self.backend.window().request_redraw();
        }
    }

    /// The row at a point in the window, in logical pixels.
    fn row_at(&self, at: Point<f64, Logical>) -> Option<usize> {
        if at.x < 0.0 || at.x >= WIDTH as f64 || at.y < HEADING as f64 {
            return None;
        }
        let row = ((at.y - HEADING as f64) / ROW as f64) as usize;
        (row < self.labels.len()).then_some(row)
    }

    fn size(&self) -> Size<i32, Physical> {
        self.backend.window_size()
    }

    fn scale(&self) -> f64 {
        self.backend.scale_factor()
    }

    /// The heading and every label drawn as textures, at the window's
    /// scale (again if it changed).
    fn textures(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let scale = self.scale();
        if self.drawn.as_ref().is_some_and(|(s, _)| *s == scale) {
            return Ok(());
        }
        let lines = std::iter::once(TITLE).chain(self.labels.iter().map(String::as_str));
        let mut textures = Vec::new();
        for line in lines {
            let line = self.text.line(line, TEXT * scale as f32);
            let size = (line.width as i32, line.height as i32);
            textures.push(self.backend.renderer().import_memory(
                &line.pixels,
                Fourcc::Argb8888,
                size.into(),
                false,
            )?);
        }
        self.drawn = Some((scale, textures));
        Ok(())
    }

    /// The thumbnails' new frames, onto their textures.
    fn take_frames(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        for at in 0..self.thumbnails.len() {
            let Some(image) = self.captures.thumbnails[at].image.take() else {
                continue;
            };
            let renderer = self.backend.renderer();
            let texture = &mut self.thumbnails[at];
            match image {
                // On the GPU already: the renderer only has to see it as a
                // texture (it remembers dmabufs it has seen).
                Image::Dmabuf(dmabuf) => *texture = Some(renderer.import_dmabuf(&dmabuf, None)?),
                Image::Pixels {
                    width,
                    height,
                    pixels,
                } => match texture {
                    // The same size as the last: into the same texture.
                    Some(t) if (t.width() as i32, t.height() as i32) == (width, height) => {
                        let all = Rectangle::from_size((width, height).into());
                        renderer.update_memory(t, &pixels, all)?;
                    }
                    _ => {
                        let size = (width, height).into();
                        let imported =
                            renderer.import_memory(&pixels, Fourcc::Xrgb8888, size, false)?;
                        *texture = Some(imported);
                    }
                },
            }
        }
        Ok(())
    }

    fn draw(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.textures()?;
        self.take_frames()?;
        let size = self.size();
        let scale = self.scale();
        let px = |logical: i32| (logical as f64 * scale).round() as i32;
        let whole = [Rectangle::from_size(size)];
        let (_, textures) = self.drawn.as_ref().ok_or("no text")?;
        let (heading, labels) = textures.split_first().ok_or("no text")?;
        let selected = self.selected;
        let thumbnails = &self.thumbnails;
        {
            let (renderer, mut framebuffer) = self.backend.bind()?;

            // Flipped180: what winit's GL surface needs to come out the
            // right way up.
            let mut frame = renderer.render(&mut framebuffer, size, Transform::Flipped180)?;
            frame.clear(BACKGROUND, &whole)?;
            line(&mut frame, &whole, heading, px(0), px(HEADING), px(PADDING))?;
            for (row, label) in labels.iter().enumerate() {
                let top = HEADING + ROW * row as i32;
                if row == selected {
                    let at = Rectangle::new(
                        (px(PADDING / 2), px(top)).into(),
                        (px(WIDTH - PADDING), px(ROW)).into(),
                    );
                    frame.draw_solid(at, &whole, SELECTED)?;
                }
                // The thumbnail at the left, in the middle of the row; the
                // label after it.
                let (w, h) = THUMBNAIL;
                let slot = Rectangle::new(
                    (px(PADDING), px(top + (ROW - h) / 2)).into(),
                    (px(w), px(h)).into(),
                );
                match &thumbnails[row] {
                    Some(t) => thumbnail(&mut frame, &whole, t, slot)?,
                    None => frame.draw_solid(slot, &whole, NO_THUMBNAIL)?,
                }
                let left = px(PADDING + w + PADDING);
                line(&mut frame, &whole, label, px(top), px(ROW), left)?;
            }

            // Showing it (submit) waits for the drawing: nothing to wait for.
            let _drawn = frame.finish()?;
        }
        self.backend.submit(None)?;
        // Drawn: the thumbnails' next frames.
        self.captures.next(&self.qh);
        Ok(())
    }
}

/// Draw a line of text in a row top pixels down and height tall: in its
/// middle, left from the left. whole: the window (all of it is drawn).
fn line(
    frame: &mut GlesFrame<'_, '_>,
    whole: &[Rectangle<i32, Physical>],
    text: &GlesTexture,
    top: i32,
    height: i32,
    left: i32,
) -> Result<(), GlesError> {
    let at = Point::from((left, top + (height - text.height() as i32) / 2));
    frame.render_texture_at(text, at, 1, 1.0, Transform::Normal, whole, &[], 1.0)
}

/// Draw a thumbnail as big as fits in slot, keeping its shape, centred.
fn thumbnail(
    frame: &mut GlesFrame<'_, '_>,
    whole: &[Rectangle<i32, Physical>],
    texture: &GlesTexture,
    slot: Rectangle<i32, Physical>,
) -> Result<(), GlesError> {
    let (w, h) = (texture.width() as f64, texture.height() as f64);
    let fit = (slot.size.w as f64 / w).min(slot.size.h as f64 / h);
    let size: Size<i32, Physical> = ((w * fit).round() as i32, (h * fit).round() as i32).into();
    let at = slot.loc + Point::from(((slot.size.w - size.w) / 2, (slot.size.h - size.h) / 2));
    let src = Rectangle::from_size(texture.size()).to_f64();
    let dst = Rectangle::new(at, size);
    // Frame's, not GlesFrame's own (which takes a shader too).
    Frame::render_texture_from_to(frame, texture, src, dst, whole, &[], Transform::Normal, 1.0)
}
