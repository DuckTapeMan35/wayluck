//! The picker, drawn by wayluck itself over one monitor: a tile for each
//! thing that can be shared, showing it live, shrunk to fit, with its name
//! under it; the one selected highlighted. While it's up it has the keyboard and the
//! pointer; no client sees either.

use crate::{
    Wayluck,
    render::{
        self, Element, argb,
        rounded::{AsGles, ClipIds},
        text::Text,
        thumbnail::Thumbnail,
    },
    windows::{self, WindowId},
    wm::scenes::{Scene, SceneId},
};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ImportAll, ImportMem, Renderer, Texture,
            element::{
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                solid::{SolidColorBuffer, SolidColorRenderElement},
            },
        },
    },
    desktop::Window,
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale, Size, Transform},
};

/// What something shows, drawn from (0, 0), and its size.
type Drawn<R> = (Vec<Element<R>>, Size<i32, Logical>);

/// A choice, its tile (global), and whether it's selected.
#[cfg(feature = "test-harness")]
type Tile = (Choice, Rectangle<i32, Logical>, bool);

/// Behind the tiles, over the whole monitor.
const BACKGROUND: u32 = 0xff15151a;
/// A tile, behind what it shows.
const TILE: u32 = 0xff2a2a33;
/// The selected tile.
const SELECTED: u32 = 0xff4a6fa5;
/// Between tiles, and around them.
const GAP: i32 = 24;
/// Around what a tile shows, inside it.
const PADDING: i32 = 12;
/// How big a tile's name is written.
const LABEL_SIZE: f64 = 15.0;
/// How tall the strip at the bottom of a tile its name is in.
const LABEL_STRIP: i32 = 28;

/// Something the user may share.
#[derive(Clone, Debug, PartialEq)]
pub enum Choice {
    Monitor(Output),
    Workspace(SceneId),
    Window(WindowId),
}

/// The picker, up on a monitor.
pub struct Shown {
    pub output: Output,
    pub choices: Vec<Choice>,
    /// The choice Enter shares (an index into choices).
    pub selected: usize,
    background: SolidColorBuffer,
    tiles: Vec<SolidColorBuffer>,
    /// Behind what each tile shows, for a monitor or a workspace: where
    /// no window is.
    backdrops: Vec<SolidColorBuffer>,
    /// The ids what each tile shows was drawn under last.
    ids: Vec<ClipIds>,
    /// Each tile's name, drawn (none without a font).
    labels: Vec<Option<Label>>,
}

/// A name, drawn: the image, at the output's scale, and its size.
struct Label {
    image: MemoryRenderBuffer,
    size: Size<i32, Logical>,
}

impl Wayluck {
    /// Put the picker up, on the monitor the pointer is on.
    pub(super) fn show_picker(&mut self) {
        let pointer = self.seat.get_pointer().unwrap().current_location();
        let on_pointer = self.monitors().find(|o| {
            (self.space.output_geometry(o)).is_some_and(|g| g.to_f64().contains(pointer))
        });
        let Some(output) = on_pointer.or_else(|| self.monitors().next()).cloned() else {
            return self.picker_gone();
        };
        let choices = self.choices();
        if self.picking.text.is_none() {
            let text = Text::new().map_err(|e| tracing::warn!("the picker has no names: {e}"));
            self.picking.text = Some(text.ok());
        }
        let labels = self.labels(&output, &choices);
        let tiles = choices
            .iter()
            .map(|_| SolidColorBuffer::new((0, 0), argb(TILE)))
            .collect();
        let ids = choices.iter().map(|_| ClipIds::new()).collect();
        let backdrops = choices
            .iter()
            .map(|_| SolidColorBuffer::new((0, 0), render::CLEAR))
            .collect();
        self.picking.shown = Some(Shown {
            output,
            choices,
            selected: 0,
            background: SolidColorBuffer::new((0, 0), argb(BACKGROUND)),
            tiles,
            backdrops,
            ids,
            labels,
        });
        self.queue_redraw();
        // Whatever client the pointer is over loses it.
        self.repoint();
    }

    /// Each choice's name, drawn to fit its tile on output.
    fn labels(&self, output: &Output, choices: &[Choice]) -> Vec<Option<Label>> {
        let area = self.space.output_geometry(output);
        let (Some(Some(text)), Some(area)) = (&self.picking.text, area) else {
            return choices.iter().map(|_| None).collect();
        };
        let scale = output.current_scale().fractional_scale();
        let rects = layout(choices.len(), area.size);
        let labels = choices.iter().zip(rects).map(|(choice, rect)| {
            let width = ((rect.size.w - 2 * PADDING) as f64 * scale).max(1.0);
            let line = text.line_within(
                &self.name(choice),
                (LABEL_SIZE * scale) as f32,
                width as usize,
            );
            let (w, h) = (line.width as i32, line.height as i32);
            let image = MemoryRenderBuffer::from_slice(
                &line.pixels,
                Fourcc::Argb8888,
                (w, h),
                1,
                Transform::Normal,
                None,
            );
            let size = Size::<i32, Physical>::from((w, h))
                .to_f64()
                .to_logical(scale)
                .to_i32_round();
            Some(Label { image, size })
        });
        labels.collect()
    }

    /// What a choice is called: a monitor by its name, a workspace by what
    /// the window manager called it, a window by its title (or app id).
    fn name(&self, choice: &Choice) -> String {
        match choice {
            Choice::Monitor(output) => output.name(),
            Choice::Workspace(scene) => (self.picking.offers.iter())
                .find(|o| o.scene == *scene)
                .map_or_else(
                    || format!("Workspace {}", scene.number()),
                    |o| o.name.clone(),
                ),
            Choice::Window(id) => {
                let names = self.windows.get(*id).map(windows::names);
                let (title, app_id) = names.unwrap_or_default();
                (title.or(app_id)).unwrap_or_else(|| format!("Window {}", u32::from(*id)))
            }
        }
    }

    /// What the picker offers, if it's up: each choice, its tile (global),
    /// and whether it's selected.
    #[cfg(feature = "test-harness")]
    pub fn picker_tiles(&self) -> Option<Vec<Tile>> {
        let shown = self.picking.shown.as_ref()?;
        let tiles = self.picker_tile_rects();
        let choices = shown.choices.iter().cloned().enumerate();
        let tiles = (choices.zip(tiles))
            .map(|((i, choice), rect)| (choice, rect, i == shown.selected))
            .collect();
        Some(tiles)
    }

    /// Where the picker's tiles are, in global coordinates: none if it
    /// isn't up.
    pub(super) fn picker_tile_rects(&self) -> Vec<Rectangle<i32, Logical>> {
        let Some(shown) = &self.picking.shown else {
            return Vec::new();
        };
        let Some(area) = self.space.output_geometry(&shown.output) else {
            return Vec::new();
        };
        let tiles = layout(shown.choices.len(), area.size).into_iter();
        tiles
            .map(|r| Rectangle::new(r.loc + area.loc, r.size))
            .collect()
    }

    /// Is the picker up (anywhere)?
    pub fn picker_shown(&self) -> bool {
        self.picking.shown.is_some()
    }

    /// What can be shared: each monitor, then the workspaces offered on it
    /// and their windows, or with none offered, the windows it shows;
    /// topmost first.
    fn choices(&self) -> Vec<Choice> {
        let mut choices = Vec::new();
        let windows = |scene: Option<&Scene>, choices: &mut Vec<Choice>| {
            let Some(scene) = scene else {
                return;
            };
            for (id, _) in scene.windows().rev() {
                // The window manager's own: a preview.
                if !self.windows.is_owned(id) && self.windows.get(id).is_some() {
                    choices.push(Choice::Window(id));
                }
            }
        };
        for monitor in self.monitors() {
            choices.push(Choice::Monitor(monitor.clone()));
            let offered: Vec<SceneId> = (self.picking.offers.iter())
                .filter(|o| o.output == *monitor && self.scenes.exists(o.scene))
                .map(|o| o.scene)
                .collect();
            if offered.is_empty() {
                windows(self.scenes.shown_on(monitor), &mut choices);
                continue;
            }
            for scene in offered {
                choices.push(Choice::Workspace(scene));
                windows(self.scenes.get(scene), &mut choices);
            }
        }
        choices
    }

    /// What the picker draws on output, front to back: in each tile what
    /// it shows, then the tile; then the background. None if it isn't up
    /// there.
    pub fn picker_elements<R>(
        &mut self,
        output: &Output,
        renderer: &mut R,
    ) -> Option<Vec<Element<R>>>
    where
        R: Renderer + ImportAll + ImportMem + AsGles,
        R::TextureId: Clone + Texture + Send + 'static,
    {
        let area = self.space.output_geometry(output)?;
        if !self
            .picking
            .shown
            .as_ref()
            .is_some_and(|s| s.output == *output)
        {
            return None;
        }
        // Out while drawing: what tiles show is drawn from the rest of self.
        let mut shown = self.picking.shown.take()?;
        let scale = Scale::from(output.current_scale().fractional_scale());
        let mut elements = Vec::new();
        let rects = layout(shown.choices.len(), area.size);
        for (i, rect) in rects.into_iter().enumerate() {
            let mut inside = Rectangle::new(
                rect.loc + Point::from((PADDING, PADDING)),
                (rect.size.w - 2 * PADDING, rect.size.h - 2 * PADDING).into(),
            );
            // Its name, in a strip at the bottom; what it shows above.
            if let Some(label) = &shown.labels[i] {
                inside.size.h -= LABEL_STRIP;
                let below = inside.loc.y + inside.size.h + (LABEL_STRIP - label.size.h) / 2;
                let at = Point::<i32, Logical>::from((inside.loc.x, below));
                let at = at.to_physical_precise_round::<_, i32>(scale).to_f64();
                let size = Some(label.size);
                let kind = Kind::Unspecified;
                let drawn = MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    at,
                    &label.image,
                    None,
                    None,
                    size,
                    kind,
                );
                elements.extend(drawn.ok().map(Element::Memory));
            }
            let drawn = self.choice_elements(&shown.choices[i], renderer, scale);
            if let Some((drawn, size)) = drawn.filter(|_| !inside.is_empty()) {
                let (shrink, fitted) = fit(size, inside);
                let at = fitted.loc.to_physical_precise_round(scale);
                let within = fitted.to_physical_precise_round(scale);
                let ids = &mut shown.ids[i];
                elements.extend(Thumbnail::many(drawn, shrink, at, within, scale, ids));
                if !matches!(shown.choices[i], Choice::Window(_)) {
                    let backdrop = &mut shown.backdrops[i];
                    backdrop.resize(fitted.size);
                    elements.push(Element::Solid(SolidColorRenderElement::from_buffer(
                        backdrop,
                        at,
                        scale,
                        1.0,
                        Kind::Unspecified,
                    )));
                }
            }
            let tile = &mut shown.tiles[i];
            let color = if i == shown.selected { SELECTED } else { TILE };
            tile.update(rect.size, argb(color));
            let at = rect.loc.to_physical_precise_round(scale);
            elements.push(Element::Solid(SolidColorRenderElement::from_buffer(
                tile,
                at,
                scale,
                1.0,
                Kind::Unspecified,
            )));
        }
        shown.background.resize(area.size);
        elements.push(Element::Solid(SolidColorRenderElement::from_buffer(
            &shown.background,
            Point::default(),
            scale,
            1.0,
            Kind::Unspecified,
        )));
        self.picking.shown = Some(shown);
        Some(elements)
    }

    /// What a choice shows, drawn from (0, 0) at scale, and its size: a
    /// monitor as it is under the picker, a window alone, a workspace's
    /// windows as they're laid out in it. None if there's nothing to draw.
    fn choice_elements<R>(
        &mut self,
        choice: &Choice,
        renderer: &mut R,
        scale: Scale<f64>,
    ) -> Option<Drawn<R>>
    where
        R: Renderer + ImportAll + ImportMem + AsGles,
        R::TextureId: Clone + Texture + Send + 'static,
    {
        let (drawn, size) = match choice {
            Choice::Monitor(output) => {
                let size = self.space.output_geometry(output)?.size;
                (render::output_elements(self, output, renderer, scale), size)
            }
            Choice::Window(id) => {
                let window = self.windows.get(*id)?.clone();
                let radius = self.windows.border_mut(*id).map_or(0, |b| b.radius);
                let at = Point::default();
                let drawn = render::window_elements(renderer, &window, at, scale, radius, None);
                (drawn, window.geometry().size)
            }
            Choice::Workspace(scene) => {
                let scene = self.scenes.get(*scene)?;
                let size = scene.size;
                // Topmost first, as drawing wants them.
                let windows: Vec<_> = (scene.windows().rev())
                    .filter_map(|(id, at)| Some((self.windows.get(id)?.clone(), id, at)))
                    .collect();
                let mut drawn = Vec::new();
                for (window, id, at) in windows {
                    let radius = self.windows.border_mut(id).map_or(0, |b| b.radius);
                    drawn.extend(render::window_elements(
                        renderer, &window, at, scale, radius, None,
                    ));
                }
                (drawn, size)
            }
        };
        (size.w > 0 && size.h > 0).then_some((drawn, size))
    }

    /// The windows the picker on output shows that may be on no monitor
    /// (in a workspace not shown): they're told when a frame of it is
    /// drawn, so they keep drawing.
    pub fn picker_windows(&self, output: &Output) -> Vec<Window> {
        let Some(shown) = self.picking.shown.as_ref().filter(|s| s.output == *output) else {
            return Vec::new();
        };
        let mut windows = Vec::new();
        for choice in &shown.choices {
            match choice {
                Choice::Window(id) => windows.extend(self.windows.get(*id).cloned()),
                Choice::Workspace(scene) => {
                    let scene = self.scenes.get(*scene);
                    for (id, _) in scene.into_iter().flat_map(|s| s.windows()) {
                        windows.extend(self.windows.get(id).cloned());
                    }
                }
                Choice::Monitor(_) => {}
            }
        }
        windows
    }
}

/// Something size big, shrunk to fit inside (keeping its shape; never made
/// bigger) and centred in it: by how much, and where it is then.
fn fit(
    size: Size<i32, Logical>,
    inside: Rectangle<i32, Logical>,
) -> (f64, Rectangle<i32, Logical>) {
    let shrink = (inside.size.w as f64 / size.w as f64)
        .min(inside.size.h as f64 / size.h as f64)
        .min(1.0);
    let w = ((size.w as f64 * shrink).round() as i32).max(1);
    let h = ((size.h as f64 * shrink).round() as i32).max(1);
    let at = inside.loc + Point::from(((inside.size.w - w) / 2, (inside.size.h - h) / 2));
    (shrink, Rectangle::new(at, (w, h).into()))
}

/// How many tiles a row of the grid has, for n.
pub(super) fn columns(n: usize) -> usize {
    (n as f64).sqrt().ceil() as usize
}

/// n tiles in a grid filling size (an output's, from its corner), as square
/// as it gets: GAP between them and around them. In order, row by row.
fn layout(n: usize, size: Size<i32, Logical>) -> Vec<Rectangle<i32, Logical>> {
    if n == 0 {
        return Vec::new();
    }
    let columns = columns(n) as i32;
    let rows = (n as i32 + columns - 1) / columns;
    let w = ((size.w - GAP * (columns + 1)) / columns).max(1);
    let h = ((size.h - GAP * (rows + 1)) / rows).max(1);
    (0..n as i32)
        .map(|i| {
            let (column, row) = (i % columns, i / columns);
            let at = (GAP + column * (w + GAP), GAP + row * (h + GAP));
            Rectangle::new(at.into(), (w, h).into())
        })
        .collect()
}
