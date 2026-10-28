//! The login screen's last picture over the new session (docs/greeter.md, step c): `--curtain DIR`
//! has one PNG per screen (DIR/<output>.png, taken by the greeter with grim); each is drawn over
//! everything until the user's shell has its first layer up, then fades, so the handover shows no
//! black frame and no half-loaded shell.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::Transform;

use crate::state::Loom;

/// how long the curtain fades once the shell is there
const FADE: Duration = Duration::from_millis(450);
/// after the shell's real frame, before the fade (a frame for it to reach the screen)
const GRACE: Duration = Duration::from_millis(100);
/// the latest it goes, shell or not (a broken shell must not hide the session)
const AT_MOST: Duration = Duration::from_secs(6);

pub struct Curtain {
    dir: PathBuf,
    started: Instant,
    fade_from: Option<Instant>,
    /// buffers the shell's wallpaper layer sent: Qt's first is often still empty, the second is its
    /// picture (headless: two dark frames when the curtain went at the first)
    seen: u32,
    pictures: RefCell<HashMap<String, Option<(MemoryRenderBuffer, (i32, i32))>>>,
}

impl Curtain {
    pub fn new(dir: PathBuf) -> Self {
        eprintln!("loom: curtain from {}", dir.display());
        Curtain { dir, started: Instant::now(), fade_from: None, seen: 0, pictures: Default::default() }
    }

    /// 1 while it covers, falling to 0 as it fades; None once gone
    pub fn alpha(&self, now: Instant) -> Option<f32> {
        let from = self.fade_from.unwrap_or(self.started + AT_MOST);
        let t = now.saturating_duration_since(from).as_secs_f32() / FADE.as_secs_f32();
        // ease out: fast at first, settling
        (t < 1.0).then(|| (1.0 - t.clamp(0.0, 1.0)).powi(3))
    }

    /// the picture of screen `name`, read once
    pub fn picture(&self, name: &str) -> Option<(MemoryRenderBuffer, (i32, i32))> {
        self.pictures
            .borrow_mut()
            .entry(name.to_string())
            .or_insert_with(|| {
                // grim writes 8-bit RGB(A) PNGs
                let dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(self.dir.join(format!("{name}.png"))).ok()?));
                let mut rd = dec.read_info().ok()?;
                // the greeter's user writes these: a picture bigger than any screen is refused, not
                // decoded (a huge one would take the new session down at its start)
                let (iw, ih) = (rd.info().width as u64, rd.info().height as u64);
                if iw == 0 || ih == 0 || iw > 16384 || ih > 16384 || iw * ih > 8192 * 8192 {
                    eprintln!("loom: curtain picture {name} refused ({iw}x{ih})");
                    return None;
                }
                let mut buf = vec![0; rd.output_buffer_size()];
                let info = rd.next_frame(&mut buf).ok()?;
                let (w, h) = (info.width, info.height);
                let px: Vec<u8> = match info.color_type {
                    // ARGB8888 in memory is B G R A
                    png::ColorType::Rgba => buf[..info.buffer_size()].chunks(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect(),
                    png::ColorType::Rgb => buf[..info.buffer_size()].chunks(3).flat_map(|p| [p[2], p[1], p[0], 255]).collect(),
                    _ => return None,
                };
                Some((MemoryRenderBuffer::from_slice(&px, Fourcc::Argb8888, (w as i32, h as i32), 1, Transform::Normal, None), (w as i32, h as i32)))
            })
            .clone()
    }
}

impl Loom {
    /// A layer drew its first frame: once the shell's wallpaper is (ii's background layer,
    /// LOOM_CURTAIN_UNTIL to name another) the curtain fades; a bar alone would show an empty screen.
    pub fn curtain_layer(&mut self, namespace: &str) {
        let Some(c) = self.curtain.as_mut() else { return };
        let until = std::env::var("LOOM_CURTAIN_UNTIL").unwrap_or_else(|_| "quickshell:background".into());
        // gone already (the shell was slower than AT_MOST): it does not come back
        if c.fade_from.is_some() || c.started.elapsed() >= AT_MOST || namespace != until {
            return;
        }
        c.seen += 1;
        if c.seen >= 2 {
            c.fade_from = Some(Instant::now() + GRACE);
            eprintln!("loom: curtain fades ({namespace} is up after {} ms)", c.started.elapsed().as_millis());
        }
    }

    /// Still drawn (frames keep coming while it is).
    pub fn curtain_animating(&self, now: Instant) -> bool {
        self.curtain.as_ref().is_some_and(|c| c.alpha(now).is_some())
    }
}
