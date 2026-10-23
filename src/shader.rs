//! Hyprland's `decoration:screen_shader`: each screen drawn once more through a fragment shader
//! (the shell's display grading and effects: vibrance, sepia, crt …). The screen is drawn into a
//! texture, then shown through the shader; the cursor stays on top as it is. Hyprland's shaders
//! are GLSL ES 3.00 (`v_texcoord`, `fragColor`, `texture`); Smithay's are 1.00, so they are turned
//! into that (`textureSize(tex, 0)` becomes the screen's size). GL only.

use std::{cell::RefCell, collections::HashMap};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::{texture::TextureRenderElement, Id, Kind, RenderElement},
            gles::{element::TextureShaderElement, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName, UniformType},
            Bind, Offscreen, Renderer,
        },
    },
    output::Output,
    utils::Transform,
};

/// A Hyprland (GLSL ES 3.00) screen shader as Smithay's GLSL 1.00 texture shader.
pub fn translate(src: &str) -> String {
    let mut out = String::from(
        "#version 100\n//_DEFINES_\n#if defined(EXTERNAL)\n#extension GL_OES_EGL_image_external : require\n#endif\nprecision highp float;\nvarying vec2 v_coords;\nuniform float alpha;\nuniform vec2 loom_size;\n#define v_texcoord v_coords\n#define fragColor gl_FragColor\n",
    );
    for line in src.lines() {
        let t = line.trim_start();
        // its version, and the in/out Smithay declares itself
        if t.starts_with("#version") || t.starts_with("in vec2 v_texcoord") || t.starts_with("out vec4 fragColor") || t.starts_with("varying vec2 v_texcoord") {
            continue;
        }
        let line = line.replace("textureSize(tex, 0)", "loom_size").replace("textureSize(tex,0)", "loom_size");
        // texture( → texture2D( (a whole word only)
        let mut fixed = String::with_capacity(line.len() + 8);
        let mut rest = line.as_str();
        while let Some(i) = rest.find("texture(") {
            let before = rest[..i].chars().last();
            fixed.push_str(&rest[..i]);
            fixed.push_str(if before.is_some_and(|c| c.is_alphanumeric() || c == '_') { "texture(" } else { "texture2D(" });
            rest = &rest[i + "texture(".len()..];
        }
        fixed.push_str(rest);
        out.push_str(&fixed);
        out.push('\n');
    }
    out
}

thread_local! {
    /// the shader file and its program (None: it did not compile, said once)
    static PROGRAM: RefCell<Option<(String, Option<GlesTexProgram>)>> = const { RefCell::new(None) };
    /// each screen's picture before the shader, by screen name
    static TARGETS: RefCell<HashMap<String, GlesTexture>> = RefCell::new(HashMap::new());
}

fn program(r: &mut GlesRenderer, path: &str) -> Option<GlesTexProgram> {
    PROGRAM.with(|p| {
        let mut p = p.borrow_mut();
        if p.as_ref().is_none_or(|(had, _)| had != path) {
            let compiled = std::fs::read_to_string(path).map_err(|e| format!("{e}")).and_then(|src| r.compile_custom_texture_shader(translate(&src), &[UniformName::new("loom_size", UniformType::_2f)]).map_err(|e| format!("{e:?}")));
            if let Err(e) = &compiled {
                eprintln!("loom: screen shader {path}: {e}");
            }
            *p = Some((path.to_string(), compiled.ok()));
        }
        p.as_ref().and_then(|(_, prog)| prog.clone())
    })
}

/// The screen's elements drawn into a texture and shown through the shader at `path`; as they
/// are when there is none (or it does not work).
pub fn apply<E: RenderElement<GlesRenderer>>(r: &mut GlesRenderer, out: Vec<E>, output: &Output, path: &str) -> Result<TextureShaderElement, Vec<E>> {
    if path.is_empty() {
        return Err(out);
    }
    let Some(prog) = program(r, path) else { return Err(out) };
    let Some(mode) = output.current_mode() else { return Err(out) };
    let (w, h) = (mode.size.w, mode.size.h);
    let name = output.name();
    let tex = TARGETS.with(|t| {
        let mut t = t.borrow_mut();
        if let Some(tex) = t.get(&name).filter(|tex| smithay::backend::renderer::Texture::size(*tex) == (w, h).into()) {
            return Some(tex.clone());
        }
        let tex: GlesTexture = r.create_buffer(Fourcc::Abgr8888, (w, h).into()).ok()?;
        t.insert(name.clone(), tex.clone());
        Some(tex)
    });
    let Some(mut tex) = tex else { return Err(out) };
    {
        let Ok(mut fb) = r.bind(&mut tex) else { return Err(out) };
        let mut damage = OutputDamageTracker::from_output(output);
        if damage.render_output(r, &mut fb, 0, &out, [0.08, 0.08, 0.1, 1.0]).is_err() {
            return Err(out);
        }
    }
    let scale = output.current_scale().fractional_scale();
    let logical = (mode.size.to_f64().to_logical(scale)).to_i32_round();
    let inner = TextureRenderElement::from_static_texture(Id::new(), r.context_id(), (0.0, 0.0), tex, 1, Transform::Normal, None, None, Some(logical), None, Kind::Unspecified);
    Ok(TextureShaderElement::new(inner, prog, vec![Uniform::new("loom_size", (w as f32, h as f32))]))
}

#[cfg(test)]
mod tests {
    #[test]
    fn turns_hyprland_shaders_into_glsl_100() {
        let src = "#version 300 es\nprecision highp float;\nin vec2 v_texcoord;\nuniform sampler2D tex;\nout vec4 fragColor;\nvoid main() {\n    vec2 px = 1.0 / vec2(textureSize(tex, 0));\n    vec4 c = texture(tex, v_texcoord);\n    fragColor = c * mytexture(1.0);\n}\n";
        let out = super::translate(src);
        assert!(out.starts_with("#version 100\n//_DEFINES_"));
        assert!(!out.contains("300 es") && !out.contains("in vec2 v_texcoord") && !out.contains("out vec4"));
        assert!(out.contains("texture2D(tex, v_texcoord)") && out.contains("mytexture(1.0)") && out.contains("vec2(loom_size)"));
    }
}
