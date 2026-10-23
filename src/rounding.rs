//! Rounded window corners: a window's surfaces drawn through a texture shader that masks the
//! corners of its geometry (anti-aliased, Hyprland's `rounding` and `rounding_power`). GL only;
//! pixman (the CPU test renderer) draws square corners. A rounded window is never put straight
//! on a plane (the plane would show the square buffer); a fullscreen one is not rounded and can be.

use smithay::{
    backend::renderer::{
        element::{surface::WaylandSurfaceRenderElement, Element, Id, Kind, RenderElement, UnderlyingStorage},
        gles::{GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform, UniformName, UniformType},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{Buffer, Physical, Rectangle, Scale, Transform},
};

const SHADER: &str = r#"
#version 100
//_DEFINES_
#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif
precision mediump float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
uniform float alpha;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif
// the window's geometry size, where this surface sits in it and how big it is (pixels)
uniform vec2 geo_size;
uniform vec2 elem_off;
uniform vec2 elem_size;
uniform float radius;
uniform float power;

void main() {
    vec4 color = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif
    // this pixel in the window's geometry; past a corner's centre: how far out of its curve
    vec2 p = elem_off + v_coords * elem_size;
    // outside the box (a client's own shadow margin): not drawn
    if (p.x < 0.0 || p.y < 0.0 || p.x > geo_size.x || p.y > geo_size.y)
        color = vec4(0.0);
    vec2 q = abs(p - geo_size * 0.5) - (geo_size * 0.5 - vec2(radius));
    if (q.x > 0.0 && q.y > 0.0) {
        float d = pow(pow(q.x, power) + pow(q.y, power), 1.0 / power);
        color *= clamp(radius - d + 0.5, 0.0, 1.0);
    }
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
"#;

/// The program, compiled once per renderer (kept in its context's user data).
struct Program(GlesTexProgram);

pub fn program(renderer: &mut GlesRenderer) -> Option<GlesTexProgram> {
    if let Some(p) = renderer.egl_context().user_data().get::<Program>() {
        return Some(p.0.clone());
    }
    let uniforms = [
        UniformName::new("geo_size", UniformType::_2f),
        UniformName::new("elem_off", UniformType::_2f),
        UniformName::new("elem_size", UniformType::_2f),
        UniformName::new("radius", UniformType::_1f),
        UniformName::new("power", UniformType::_1f),
    ];
    match renderer.compile_custom_texture_shader(SHADER, &uniforms) {
        Ok(p) => {
            renderer.egl_context().user_data().insert_if_missing(|| Program(p.clone()));
            Some(p)
        }
        Err(e) => {
            eprintln!("loom: the rounding shader did not compile: {e:?}");
            None
        }
    }
}

/// One surface of a window, its corners masked by the window's geometry.
pub struct Rounded {
    pub inner: WaylandSurfaceRenderElement<GlesRenderer>,
    /// the window's geometry size, and where this surface sits in it (pixels; fixed when made, so
    /// a moved copy of the element, e.g. a screenshot region, keeps its corners)
    pub geo_size: (f32, f32),
    pub off: (f32, f32),
    pub radius: f32,
    pub power: f32,
    pub program: GlesTexProgram,
}

impl Element for Rounded {
    fn id(&self) -> &Id {
        self.inner.id()
    }
    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }
    fn transform(&self) -> Transform {
        self.inner.transform()
    }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }
    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }
    // the corners are see-through now: nothing counts as opaque (what is behind gets drawn)
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }
    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }
    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for Rounded {
    fn draw(&self, frame: &mut GlesFrame<'_, '_>, src: Rectangle<f64, Buffer>, dst: Rectangle<i32, Physical>, damage: &[Rectangle<i32, Physical>], opaque: &[Rectangle<i32, Physical>]) -> Result<(), GlesError> {
        let uniforms = vec![
            Uniform::new("geo_size", self.geo_size),
            Uniform::new("elem_off", self.off),
            Uniform::new("elem_size", (dst.size.w as f32, dst.size.h as f32)),
            Uniform::new("radius", self.radius),
            Uniform::new("power", self.power),
        ];
        frame.override_default_tex_program(self.program.clone(), uniforms);
        let r = self.inner.draw(frame, src, dst, damage, opaque);
        frame.clear_tex_program_override();
        r
    }
    // never straight onto a plane: the shader would be skipped
    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

impl Rounded {
    /// `inner` masked by window geometry `geo` (both in the output's pixels at `scale`).
    pub fn new(inner: WaylandSurfaceRenderElement<GlesRenderer>, geo: Rectangle<i32, Physical>, scale: Scale<f64>, radius: f32, power: f32, program: GlesTexProgram) -> Rounded {
        let at = inner.geometry(scale).loc;
        Rounded { geo_size: (geo.size.w as f32, geo.size.h as f32), off: ((at.x - geo.loc.x) as f32, (at.y - geo.loc.y) as f32), inner, radius, power, program }
    }
}
