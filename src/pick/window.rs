//! The picker's window: a list to choose from, each with a live thumbnail,
//! drawn on the GPU (smithay's renderer, through winit, as a
//! Wayland client). Fixed size, so the window manager floats it; a long
//! list scrolls under its heading. Up and Down (or the pointer) select,
//! Enter (or a click) chooses, Esc (or closing the window) cancels.

use super::{
    ask::Host,
    capture::{Captures, Image},
    text::Text,
};
use smithay::{
    backend::{
        allocator::Fourcc,
        input::{
            AbsolutePositionEvent, Axis, ButtonState, InputEvent, KeyState, KeyboardKeyEvent,
            PointerAxisEvent, PointerButtonEvent,
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
    utils::{Buffer, Logical, Physical, Point, Rectangle, Size, Transform},
};
use std::os::fd::AsFd;
use wayland_client::{EventQueue, QueueHandle};

/// The window's app id: what the window manager knows it by.
const APP_ID: &str = "wayluck.picker";
const TITLE: &str = "Choose what to share";

// Its layout, in logical pixels: a heading, then a row for each choice.
const WIDTH: i32 = 480;
/// As tall as it gets: more rows scroll.
const MAX_HEIGHT: i32 = 560;
const PADDING: i32 = 12;
const HEADING: i32 = 44;
const ROW: i32 = 96;
/// How far in a row under another goes, each level.
const INDENT: i32 = 24;
/// The box a thumbnail fits in, keeping its shape: 16:9.
const THUMBNAIL: (i32, i32) = (128, 72);
/// How big thumbnails' frames are: twice their box, so they're sharp on
/// monitors scaled up to 2.
pub const THUMBNAIL_FRAMES: (u32, u32) = (THUMBNAIL.0 as u32 * 2, THUMBNAIL.1 as u32 * 2);
const TEXT: f32 = 15.0;
/// How far one notch of a mouse wheel scrolls.
const NOTCH: f64 = ROW as f64 / 2.0;

const BACKGROUND: Color32F = Color32F::new(0.12, 0.12, 0.13, 1.0);
const SELECTED: Color32F = Color32F::new(0.23, 0.43, 0.65, 1.0);
/// Where a thumbnail will be, until its first frame.
const NO_THUMBNAIL: Color32F = Color32F::new(0.2, 0.2, 0.22, 1.0);
const SCROLLBAR: Color32F = Color32F::new(0.35, 0.35, 0.38, 1.0);

// Keys, as xkb keycodes (evdev's + 8): the same whatever the layout.
const KEY_ESC: u32 = 1 + 8;
const KEY_ENTER: u32 = 28 + 8;
const KEY_KP_ENTER: u32 = 96 + 8;
const KEY_UP: u32 = 103 + 8;
const KEY_DOWN: u32 = 108 + 8;
const BTN_LEFT: u32 = 0x110;

/// One row, as the user sees it.
#[derive(Clone)]
pub struct Row {
    pub label: String,
    /// How far in it is: under a monitor 1, under a workspace 2.
    pub depth: u8,
    /// Can it be chosen? (A heading can't.)
    pub choosable: bool,
}

struct Picker {
    backend: WinitGraphicsBackend<GlesRenderer>,
    text: Text,
    rows: Vec<Row>,
    /// The window's height, in logical pixels.
    height: i32,
    /// How far down the list is scrolled, in logical pixels.
    scroll: i32,
    /// The heading and labels drawn, for the scale they were drawn at.
    drawn: Option<(f64, Vec<GlesTexture>)>,
    /// Where the answer goes; whoever asked may go meanwhile.
    host: Host,
    captures: Captures,
    /// To ask for the captures' next frames.
    qh: QueueHandle<Captures>,
    /// Each row's latest frame, once there is one.
    thumbnails: Vec<Option<GlesTexture>>,
    selected: usize,
    /// Where the pointer is in the window, if it is.
    pointer: Option<Point<f64, Logical>>,
    /// Some once the user decides: Some(row) chosen, None cancelled.
    answer: Option<Option<usize>>,
}

/// Show rows, with captures' thumbnails: the row the user chooses, or None
/// if they cancel (or whoever asked goes); and the host, to answer by.
pub fn choose(
    rows: Vec<Row>,
    host: Host,
    captures: Captures,
    mut queue: EventQueue<Captures>,
) -> Result<(Option<usize>, Host), Box<dyn std::error::Error>> {
    let text = Text::new()?;
    let height = (HEADING + ROW * rows.len() as i32 + PADDING).min(MAX_HEIGHT);
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
            // Nobody's asking any more: nothing to choose.
            if picker.host.closed() {
                picker.answer = Some(None);
            }
            Ok(PostAction::Continue)
        })?;
    let mut picker = Picker {
        backend,
        text,
        thumbnails: rows.iter().map(|_| None).collect(),
        rows,
        host,
        height,
        scroll: 0,
        drawn: None,
        captures,
        qh,
        selected: 0,
        pointer: None,
        answer: None,
    };
    while picker.answer.is_none() {
        event_loop.dispatch(None, &mut picker)?;
    }
    Ok((picker.answer.flatten(), picker.host))
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
                // Only a pointer moving over it selects: not one the
                // window opened under, nor one told again where it is.
                let moved = self.pointer.is_some_and(|p| p != at);
                self.pointer = Some(at);
                if moved {
                    self.select_hovered();
                }
            }
            WinitEvent::Input(InputEvent::PointerAxis { event }) => {
                let by = (event.amount(Axis::Vertical))
                    .or_else(|| event.amount_v120(Axis::Vertical).map(|v| v / 120.0 * NOTCH))
                    .unwrap_or(0.0);
                self.scroll_to(self.scroll + by.round() as i32);
                self.select_hovered();
            }
            WinitEvent::Input(InputEvent::PointerButton { event }) => {
                if event.button_code() == BTN_LEFT
                    && event.state() == ButtonState::Pressed
                    && let Some(row) = self.hovered()
                {
                    self.answer = Some(Some(row));
                }
            }
            _ => {}
        }
    }

    fn key(&mut self, key: u32) {
        let at = self.selected;
        // The nearest row that can be chosen, that way.
        let row = match key {
            KEY_ESC => return self.answer = Some(None),
            KEY_ENTER | KEY_KP_ENTER => return self.answer = Some(Some(self.selected)),
            KEY_UP => (0..at).rev().find(|r| self.rows[*r].choosable),
            KEY_DOWN => (at + 1..self.rows.len()).find(|r| self.rows[*r].choosable),
            _ => return,
        };
        let Some(row) = row else {
            // None further up: still show the headings above.
            if key == KEY_UP {
                self.scroll_to(0);
            }
            return;
        };
        self.select(row);
        self.reveal(row);
    }

    fn select(&mut self, row: usize) {
        if row != self.selected {
            self.selected = row;
            self.backend.window().request_redraw();
        }
    }

    /// The row under the pointer is selected (if it's over one).
    fn select_hovered(&mut self) {
        if let Some(row) = self.hovered() {
            self.select(row);
        }
    }

    /// Scroll as little as shows all of row.
    fn reveal(&mut self, row: usize) {
        let top = ROW * row as i32;
        if top < self.scroll {
            self.scroll_to(top);
        } else if top + ROW > self.scroll + self.list_height() {
            self.scroll_to(top + ROW - self.list_height());
        }
    }

    /// Scroll to (as far as the list goes).
    fn scroll_to(&mut self, to: i32) {
        let most = (ROW * self.rows.len() as i32 - self.list_height()).max(0);
        let to = to.clamp(0, most);
        if to != self.scroll {
            self.scroll = to;
            self.backend.window().request_redraw();
        }
    }

    /// How tall the part of the window showing the list is.
    fn list_height(&self) -> i32 {
        self.height - HEADING - PADDING
    }

    /// The row under the pointer, if any. (and it can be chosen)
    fn hovered(&self) -> Option<usize> {
        let at = self.pointer?;
        let in_list = at.y >= HEADING as f64 && at.y < (HEADING + self.list_height()) as f64;
        if at.x < 0.0 || at.x >= WIDTH as f64 || !in_list {
            return None;
        }
        let row = ((at.y - HEADING as f64 + self.scroll as f64) / ROW as f64) as usize;
        (self.rows.get(row)?.choosable).then_some(row)
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
        let lines = std::iter::once(TITLE).chain(self.rows.iter().map(|r| r.label.as_str()));
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
        let (selected, scroll, height) = (self.selected, self.scroll, self.height);
        let list_height = self.list_height();
        let (rows, thumbnails) = (&self.rows, &self.thumbnails);
        {
            let (renderer, mut framebuffer) = self.backend.bind()?;
            // Flipped180: what winit's GL surface needs to come out the
            // right way up.
            let mut frame = renderer.render(&mut framebuffer, size, Transform::Flipped180)?;
            frame.clear(BACKGROUND, &whole)?;
            for (row, label) in labels.iter().enumerate() {
                let top = HEADING + ROW * row as i32 - scroll;
                // Out of sight: not drawn.
                if top + ROW <= HEADING || top >= height {
                    continue;
                }
                if row == selected {
                    let at = Rectangle::new(
                        (px(PADDING / 2), px(top)).into(),
                        (px(WIDTH - PADDING), px(ROW)).into(),
                    );
                    frame.draw_solid(at, &whole, SELECTED)?;
                }
                // The thumbnail at the left (further in the deeper the
                // row), in the middle of the row; the label after it, cut
                // off at the window's edge.
                let left = PADDING + INDENT * rows[row].depth as i32;
                let (w, h) = THUMBNAIL;
                let slot = Rectangle::new(
                    (px(left), px(top + (ROW - h) / 2)).into(),
                    (px(w), px(h)).into(),
                );
                match &thumbnails[row] {
                    Some(t) => thumbnail(&mut frame, &whole, t, slot)?,
                    None => frame.draw_solid(slot, &whole, NO_THUMBNAIL)?,
                }
                let left = left + w + PADDING;
                let room = px(WIDTH - PADDING - left);
                line(&mut frame, &whole, label, px(top), px(ROW), px(left), room)?;
            }
            // The heading, over rows scrolled under it, and the padding at
            // the bottom.
            let top = Rectangle::from_size((size.w, px(HEADING)).into());
            frame.draw_solid(top, &whole, BACKGROUND)?;
            let bottom = Rectangle::new(
                (0, px(height - PADDING)).into(),
                (size.w, px(PADDING)).into(),
            );
            frame.draw_solid(bottom, &whole, BACKGROUND)?;
            let (left, room) = (px(PADDING), px(WIDTH - 2 * PADDING));
            line(&mut frame, &whole, heading, 0, px(HEADING), left, room)?;
            // How far down the list is, if it doesn't all fit.
            let all = ROW * rows.len() as i32;
            if all > list_height {
                let bar = Rectangle::new(
                    (px(WIDTH - 4), px(HEADING + scroll * list_height / all)).into(),
                    (px(3), px(list_height * list_height / all)).into(),
                );
                frame.draw_solid(bar, &whole, SCROLLBAR)?;
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
/// middle, left from the left, as much of it as fits in room. whole: the
/// window (all of it is drawn).
fn line(
    frame: &mut GlesFrame<'_, '_>,
    whole: &[Rectangle<i32, Physical>],
    text: &GlesTexture,
    top: i32,
    height: i32,
    left: i32,
    room: i32,
) -> Result<(), GlesError> {
    let size: Size<i32, Physical> = ((text.width() as i32).min(room), text.height() as i32).into();
    let at = Point::from((left, top + (height - size.h) / 2));
    let src: Rectangle<f64, Buffer> = Rectangle::from_size((size.w, size.h).into()).to_f64();
    let dst = Rectangle::new(at, size);
    // Frame's, not GlesFrame's own (which takes a shader too).
    Frame::render_texture_from_to(frame, text, src, dst, whole, &[], Transform::Normal, 1.0)
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
