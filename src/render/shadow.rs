//! Shadows below windows.

use super::{
    argb,
    shaders::{AsGles, ShaderElement, Shaders},
};
use smithay::{
    backend::renderer::{
        element::Kind,
        gles::{Uniform, element::PixelShaderElement},
    },
    utils::{Logical, Point, Rectangle},
};

/// What a window manager asks a window's shadow to be.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Spec {
    /// How far it's blurred, each way, in pixels.
    pub blur: u32,
    /// How much bigger than the window and its border it is, each side.
    pub spread: i32,
    /// Where it is, from below them.
    pub offset: Point<i32, Logical>,
    /// 0xAARRGGBB.
    pub color: u32,
}

/// A window's shadow.
pub struct Shadow {
    spec: Spec,
    /// How round the corners of what it's below were, last drawn.
    radius: u32,
    /// Drawn by the shadow shader: kept from frame to frame, so the damage
    /// tracker knows when it really changed. Made when it's first drawn.
    shader: Option<PixelShaderElement>,
    /// What the shader draws changed since it was told.
    changed: bool,
}

impl Shadow {
    pub fn new(spec: Spec) -> Self {
        Self {
            spec,
            radius: 0,
            shader: None,
            changed: true,
        }
    }

    pub fn set(&mut self, spec: Spec) {
        if self.spec != spec {
            self.spec = spec;
            self.changed = true;
        }
    }

    /// It below what covers `around` (a window and its border, in global
    /// coordinates; its corners rounded radius pixels), on an output whose
    /// top left is origin. None if it has no colour, or no size left.
    pub fn element<R: AsGles>(
        &mut self,
        renderer: &mut R,
        around: Rectangle<i32, Logical>,
        radius: u32,
        origin: Point<i32, Logical>,
    ) -> Option<ShaderElement> {
        let Spec {
            blur,
            spread,
            offset,
            color,
        } = self.spec;
        if color >> 24 == 0 {
            return None;
        }
        // The shape it's a blur of, and as far as the blur reaches.
        let shape = Rectangle::new(
            around.loc - origin + offset - Point::from((spread, spread)),
            (around.size.w + 2 * spread, around.size.h + 2 * spread).into(),
        );
        if shape.size.w <= 0 || shape.size.h <= 0 {
            return None;
        }
        let area = Rectangle::new(
            shape.loc - Point::from((blur as i32, blur as i32)),
            (
                shape.size.w + 2 * blur as i32,
                shape.size.h + 2 * blur as i32,
            )
                .into(),
        );
        if self.radius != radius {
            self.radius = radius;
            self.changed = true;
        }
        let uniforms = vec![
            Uniform::new("blur", blur as f32),
            Uniform::new("spread", spread as f32),
            Uniform::new("offset", [offset.x as f32, offset.y as f32]),
            Uniform::new("radius", radius as f32),
            Uniform::new("color", argb(color).components()),
        ];
        match &mut self.shader {
            Some(shader) => {
                shader.resize(area, None);
                if self.changed {
                    shader.update_uniforms(uniforms);
                }
            }
            None => {
                let program = Shaders::get(renderer)?.shadow;
                let shader =
                    PixelShaderElement::new(program, area, None, 1.0, uniforms, Kind::Unspecified);
                self.shader = Some(shader);
            }
        }
        self.changed = false;
        self.shader.clone().map(ShaderElement)
    }
}
