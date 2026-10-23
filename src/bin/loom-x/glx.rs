//! GLX for direct rendering: Mesa draws on the client side (DRI3 + Present); the server answers versions, strings, configs, contexts and GLX drawables.

use std::collections::HashMap;

use x11rb_protocol::protocol::Request;

use crate::server::{Rep, Server, VISUAL, VISUAL32};

// GLX attribute names
const GLX_BUFFER_SIZE: u32 = 2;
const GLX_LEVEL: u32 = 3;
const GLX_RGBA: u32 = 4;
const GLX_DOUBLEBUFFER: u32 = 5;
const GLX_STEREO: u32 = 6;
const GLX_AUX_BUFFERS: u32 = 7;
const GLX_RED_SIZE: u32 = 8;
const GLX_GREEN_SIZE: u32 = 9;
const GLX_BLUE_SIZE: u32 = 10;
const GLX_ALPHA_SIZE: u32 = 11;
const GLX_DEPTH_SIZE: u32 = 12;
const GLX_STENCIL_SIZE: u32 = 13;
const GLX_CONFIG_CAVEAT: u32 = 0x20;
const GLX_X_VISUAL_TYPE: u32 = 0x22;
const GLX_TRANSPARENT_TYPE: u32 = 0x23;
const GLX_NONE: u32 = 0x8000;
const GLX_TRUE_COLOR: u32 = 0x8002;
const GLX_VISUAL_ID: u32 = 0x800b;
const GLX_DRAWABLE_TYPE: u32 = 0x8010;
const GLX_RENDER_TYPE: u32 = 0x8011;
const GLX_X_RENDERABLE: u32 = 0x8012;
const GLX_FBCONFIG_ID: u32 = 0x8013;
const GLX_MAX_PBUFFER_WIDTH: u32 = 0x8016;
const GLX_MAX_PBUFFER_HEIGHT: u32 = 0x8017;
const GLX_MAX_PBUFFER_PIXELS: u32 = 0x8018;
const GLX_SAMPLE_BUFFERS: u32 = 100000;
const GLX_SAMPLES: u32 = 100001;
const GLX_SCREEN: u32 = 0x800c;
const GLX_WIDTH: u32 = 0x801d;
const GLX_HEIGHT: u32 = 0x801e;
const GLX_Y_INVERTED_EXT: u32 = 0x20d4;
const GLX_BIND_TO_TEXTURE_RGB_EXT: u32 = 0x20d0;
const GLX_BIND_TO_TEXTURE_RGBA_EXT: u32 = 0x20d1;
const GLX_BIND_TO_TEXTURE_TARGETS_EXT: u32 = 0x20d3;
const GLX_VENDOR_NAMES_EXT: u32 = 0x20f6;

const EXTENSIONS: &str = "GLX_ARB_create_context GLX_ARB_create_context_no_error GLX_ARB_create_context_profile GLX_ARB_create_context_robustness GLX_ARB_fbconfig_float GLX_ARB_framebuffer_sRGB GLX_ARB_multisample GLX_EXT_create_context_es2_profile GLX_EXT_create_context_es_profile GLX_EXT_fbconfig_packed_float GLX_EXT_framebuffer_sRGB GLX_EXT_import_context GLX_EXT_libglvnd GLX_EXT_no_config_context GLX_EXT_texture_from_pixmap GLX_EXT_visual_info GLX_EXT_visual_rating GLX_MESA_copy_sub_buffer GLX_OML_swap_method GLX_SGI_make_current_read GLX_SGIS_multisample GLX_SGIX_fbconfig GLX_SGIX_pbuffer GLX_SGIX_visual_select_group";

/// One framebuffer config: its visual, alpha, double buffering, depth and stencil.
struct Config {
    id: u32,
    visual: u32,
    alpha: bool,
    double: bool,
    depth: u32,
    stencil: u32,
}

const CONFIGS: [Config; 6] = [
    Config { id: 0x101, visual: VISUAL, alpha: false, double: true, depth: 24, stencil: 8 },
    Config { id: 0x102, visual: VISUAL, alpha: false, double: false, depth: 24, stencil: 8 },
    Config { id: 0x103, visual: VISUAL, alpha: false, double: true, depth: 0, stencil: 0 },
    Config { id: 0x104, visual: VISUAL32, alpha: true, double: true, depth: 24, stencil: 8 },
    Config { id: 0x105, visual: VISUAL32, alpha: true, double: false, depth: 24, stencil: 8 },
    Config { id: 0x106, visual: VISUAL32, alpha: true, double: true, depth: 0, stencil: 0 },
];

impl Config {
    /// The attributes as Xwayland sends them (same set and order, which Mesa matches its driver's configs against).
    fn attribs(&self) -> Vec<(u32, u32)> {
        const DONT_CARE: u32 = 0xffffffff;
        vec![
            (GLX_VISUAL_ID, self.visual),
            (GLX_FBCONFIG_ID, self.id),
            (GLX_X_RENDERABLE, 1),
            (GLX_RGBA, 1),
            (GLX_RENDER_TYPE, 1),
            (GLX_DOUBLEBUFFER, self.double as u32),
            (GLX_STEREO, 0),
            (GLX_BUFFER_SIZE, 32),
            (GLX_LEVEL, 0),
            (GLX_AUX_BUFFERS, 0),
            (GLX_RED_SIZE, 8),
            (GLX_GREEN_SIZE, 8),
            (GLX_BLUE_SIZE, 8),
            // Mesa's driver configs are ARGB, also behind the 24-bit visual
            (GLX_ALPHA_SIZE, 8),
            (0xe, 0),
            (0xf, 0),
            (0x10, 0),
            (0x11, 0),
            (GLX_DEPTH_SIZE, self.depth),
            (GLX_STENCIL_SIZE, self.stencil),
            (GLX_X_VISUAL_TYPE, GLX_TRUE_COLOR),
            (GLX_CONFIG_CAVEAT, GLX_NONE),
            (GLX_TRANSPARENT_TYPE, GLX_NONE),
            (0x25, 0),
            (0x26, 0),
            (0x27, 0),
            (0x28, 0),
            (0x24, 0),
            // swap method: undefined
            (0x8060, 0x8063),
            (GLX_SAMPLES, 0),
            (GLX_SAMPLE_BUFFERS, 0),
            (0x8028, 0),
            (GLX_DRAWABLE_TYPE, 7),
            (GLX_BIND_TO_TEXTURE_RGB_EXT, 1),
            (GLX_BIND_TO_TEXTURE_RGBA_EXT, 1),
            (0x20d2, 0),
            (GLX_BIND_TO_TEXTURE_TARGETS_EXT, DONT_CARE),
            (GLX_Y_INVERTED_EXT, DONT_CARE),
            (GLX_MAX_PBUFFER_WIDTH, 0),
            (GLX_MAX_PBUFFER_HEIGHT, 0),
            (GLX_MAX_PBUFFER_PIXELS, 0),
            (0x8019, 0),
            (0x801a, 0),
            (0, 0),
        ]
    }
}

#[derive(Default)]
pub struct Glx {
    /// context id -> its config (direct contexts: nothing more to keep)
    contexts: HashMap<u32, u32>,
    /// GLX drawable id -> the X drawable it stands for
    pub drawables: HashMap<u32, u32>,
    next_tag: u32,
}

/// A GLX string reply: pad, the length, pad, the string (NUL-terminated).
fn string_reply(seq: u16, s: &str) -> Rep {
    let mut b = s.as_bytes().to_vec();
    b.push(0);
    Rep::new(seq, 0).u32(0).u32(b.len() as u32).pad(16).bytes(&b)
}

fn attrib_reply(seq: u16, pairs: &[(u32, u32)]) -> Rep {
    let mut r = Rep::new(seq, 0).u32(pairs.len() as u32).pad(20);
    for &(k, v) in pairs {
        r = r.u32(k).u32(v);
    }
    r
}

impl Server {
    pub fn handle_glx<'a>(&mut self, idx: usize, seq: u16, major: u8, req: Request<'a>) -> Result<(), Request<'a>> {
        match req {
            Request::GlxQueryVersion(_) => self.reply(idx, Rep::new(seq, 0).u32(1).u32(4)),
            Request::GlxQueryServerString(r) => {
                let s = match r.name {
                    1 => "loom".to_string(),
                    2 => "1.4".to_string(),
                    3 => EXTENSIONS.to_string(),
                    // glvnd picks the vendor library by this
                    GLX_VENDOR_NAMES_EXT => "mesa".to_string(),
                    _ => String::new(),
                };
                self.reply(idx, string_reply(seq, &s));
            }
            Request::GlxQueryExtensionsString(_) => self.reply(idx, string_reply(seq, EXTENSIONS)),
            Request::GlxClientInfo(_) | Request::GlxSetClientInfoARB(_) | Request::GlxSetClientInfo2ARB(_) => {}
            Request::GlxGetVisualConfigs(_) => {
                // one config per visual, in GLX 1.2's 18 fixed values, then caveat as a pair
                let mut vals = Vec::new();
                for c in CONFIGS.iter().filter(|c| c.double && c.depth > 0) {
                    let a = if c.alpha { 8 } else { 0 };
                    let _ = a;
                    // the 18 GLX 1.2 values, then pairs as Xwayland sends them (40 in all)
                    vals.extend([c.visual, 4, 1, 8, 8, 8, 8, 0, 0, 0, 0, 1, 0, 32, c.depth, c.stencil, 0, 0]);
                    vals.extend([GLX_CONFIG_CAVEAT, GLX_NONE, GLX_TRANSPARENT_TYPE, GLX_NONE, 0x25, 0, 0x26, 0, 0x27, 0, 0x28, 0, 0x24, 0, GLX_SAMPLES, 0, GLX_SAMPLE_BUFFERS, 0, 0x8028, 0, 0, 0]);
                }
                let n = vals.len() / 40;
                let mut r = Rep::new(seq, 0).u32(n as u32).u32(40).pad(16);
                for v in vals {
                    r = r.u32(v);
                }
                self.reply(idx, r);
            }
            Request::GlxGetFBConfigs(_) => {
                let n_props = CONFIGS[0].attribs().len();
                let mut r = Rep::new(seq, 0).u32(CONFIGS.len() as u32).u32(n_props as u32).pad(16);
                for c in &CONFIGS {
                    for (k, v) in c.attribs() {
                        r = r.u32(k).u32(v);
                    }
                }
                self.reply(idx, r);
            }
            Request::GlxCreateContext(r) => {
                let cfg = CONFIGS.iter().find(|c| c.visual == r.visual).map(|c| c.id).unwrap_or(0x101);
                self.glx.contexts.insert(r.context, cfg);
            }
            Request::GlxCreateNewContext(r) => {
                self.glx.contexts.insert(r.context, r.fbconfig);
            }
            Request::GlxCreateContextAttribsARB(r) => {
                self.glx.contexts.insert(r.context, r.fbconfig);
            }
            Request::GlxDestroyContext(r) => {
                self.glx.contexts.remove(&r.context);
            }
            Request::GlxIsDirect(_) => self.reply(idx, Rep::new(seq, 0).u8(1)),
            Request::GlxQueryContext(r) => {
                let cfg = self.glx.contexts.get(&r.context).copied().unwrap_or(0x101);
                self.reply(idx, attrib_reply(seq, &[(GLX_FBCONFIG_ID, cfg), (GLX_RENDER_TYPE, 0x8014), (GLX_SCREEN, 0)]));
            }
            // direct contexts are made current on the client side; a tag keeps indirect-minded callers happy
            Request::GlxMakeCurrent(_) | Request::GlxMakeContextCurrent(_) => {
                self.glx.next_tag += 1;
                let t = self.glx.next_tag;
                self.reply(idx, Rep::new(seq, 0).u32(t));
            }
            Request::GlxCreateWindow(r) => {
                self.glx.drawables.insert(r.glx_window, r.window);
            }
            Request::GlxCreatePixmap(r) => {
                self.glx.drawables.insert(r.glx_pixmap, r.pixmap);
            }
            Request::GlxCreateGLXPixmap(r) => {
                self.glx.drawables.insert(r.glx_pixmap, r.pixmap);
            }
            Request::GlxCreatePbuffer(r) => {
                self.glx.drawables.insert(r.pbuffer, 0);
            }
            Request::GlxDeleteWindow(r) => {
                self.glx.drawables.remove(&r.glxwindow);
            }
            Request::GlxDestroyPixmap(r) => {
                self.glx.drawables.remove(&r.glx_pixmap);
            }
            Request::GlxDestroyGLXPixmap(r) => {
                self.glx.drawables.remove(&r.glx_pixmap);
            }
            Request::GlxDestroyPbuffer(r) => {
                self.glx.drawables.remove(&r.pbuffer);
            }
            Request::GlxGetDrawableAttributes(r) => {
                let d = self.glx.drawables.get(&r.drawable).copied().unwrap_or(r.drawable);
                let (w, h) = self.drawable_size_any(d);
                self.reply(idx, attrib_reply(seq, &[(GLX_WIDTH, w), (GLX_HEIGHT, h), (GLX_FBCONFIG_ID, 0x101), (GLX_Y_INVERTED_EXT, 0)]));
            }
            Request::GlxChangeDrawableAttributes(_) | Request::GlxWaitGL(_) | Request::GlxWaitX(_) | Request::GlxFlush(_) | Request::GlxFinish(_) | Request::GlxSwapBuffers(_) | Request::GlxUseXFont(_) | Request::GlxCopyContext(_) => {}
            // indirect GL (rendering commands through the X server) is not offered
            Request::GlxRender(_) | Request::GlxRenderLarge(_) => self.error(idx, 1, 0, major, 1),
            other => return Err(other),
        }
        Ok(())
    }
}
