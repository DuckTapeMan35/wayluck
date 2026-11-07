//! Something drawn small: elements shrunk, moved and cut to a rectangle,
//! under ids of their own. The same surface may be in several thumbnails
//! at once (a window, and the monitor it's on), and the damage tracker
//! tells elements apart by id.

use super::{
    Element,
    rounded::{AsGles, ClipIds},
};
use smithay::{
    backend::renderer::{
        ImportAll, ImportMem, Renderer,
        element::{
            Element as _, Id, Kind, RenderElement, UnderlyingStorage,
            utils::{CropRenderElement, Relocate, RelocateRenderElement, RescaleRenderElement},
        },
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{Buffer, Physical, Point, Rectangle, Scale, Transform, user_data::UserDataMap},
};

type Inner<R> = CropRenderElement<RelocateRenderElement<RescaleRenderElement<Element<R>>>>;

pub struct Thumbnail<R: Renderer> {
    // Boxed: an Element can be a thumbnail of Elements.
    inner: Box<Inner<R>>,
    id: Id,
}

impl<R> Thumbnail<R>
where
    R: Renderer + ImportAll + ImportMem + AsGles,
    R::TextureId: 'static,
{
    /// elements (drawn from (0, 0)) shrunk by shrink, their top left put at
    /// `at`, and cut to `within`: all at scale. ids: the ids they were drawn
    /// under last time, kept for those still there.
    pub fn many(
        elements: Vec<Element<R>>,
        shrink: f64,
        at: Point<i32, Physical>,
        within: Rectangle<i32, Physical>,
        scale: Scale<f64>,
        ids: &mut ClipIds,
    ) -> Vec<Element<R>> {
        let mut old = std::mem::take(ids);
        elements
            .into_iter()
            .filter_map(|e| {
                let own = e.id().clone();
                let shrunk = RescaleRenderElement::from_element(e, Point::default(), shrink);
                let moved = RelocateRenderElement::from_element(shrunk, at, Relocate::Relative);
                let cut = CropRenderElement::from_element(moved, scale, within)?;
                let id = old.remove(&own).unwrap_or_else(Id::new);
                ids.insert(own, id.clone());
                Some(Element::Thumbnail(Thumbnail {
                    inner: Box::new(cut),
                    id,
                }))
            })
            .collect()
    }
}

impl<R> smithay::backend::renderer::element::Element for Thumbnail<R>
where
    R: Renderer + ImportAll + ImportMem + AsGles,
    R::TextureId: 'static,
{
    fn id(&self) -> &Id {
        &self.id
    }
    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }
    fn transform(&self) -> Transform {
        self.inner.transform()
    }
    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.inner.opaque_regions(scale)
    }
    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }
    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl<R> RenderElement<R> for Thumbnail<R>
where
    R: Renderer + ImportAll + ImportMem + AsGles,
    R::TextureId: 'static,
{
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), R::Error> {
        self.inner
            .draw(frame, src, dst, damage, opaque_regions, cache)
    }

    /// None: drawn small, it's never put straight on a plane.
    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}
