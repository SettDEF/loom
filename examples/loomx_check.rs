//! A check of loom-x's drawing (core, RENDER compositing, transforms, glyphs), read back with GetImage. Usage: DISPLAY=:N cargo run --example loomx_check

use x11rb::{
    connection::Connection,
    protocol::{
        render::{self, ConnectionExt as _, PictOp},
        xproto::{ConnectionExt as _, CreateGCAux, ImageFormat, Rectangle},
    },
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (c, screen) = x11rb::connect(None)?;
    let root = c.setup().roots[screen].root;
    let mut failed = 0;
    let mut check = |name: &str, got: (u8, u8, u8), want: (u8, u8, u8)| {
        let close = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 3;
        let ok = close(got.0, want.0) && close(got.1, want.1) && close(got.2, want.2);
        println!("{} {name}: got {:?}, want {:?}", if ok { "PASS" } else { "FAIL" }, got, want);
        if !ok {
            failed += 1;
        }
    };

    // the picture formats by depth
    let formats = c.render_query_pict_formats()?.reply()?;
    let by_depth = |d: u8| formats.formats.iter().find(|f| f.depth == d).map(|f| f.id).unwrap();
    let (rgb, a8) = (by_depth(24), by_depth(8));

    // a 4x4 image, red left, blue right (core drawing)
    let src = c.generate_id()?;
    c.create_pixmap(24, src, root, 4, 4)?;
    let gc = c.generate_id()?;
    c.create_gc(gc, src, &CreateGCAux::new().foreground(0xff0000))?;
    c.poly_fill_rectangle(src, gc, &[Rectangle { x: 0, y: 0, width: 2, height: 4 }])?;
    c.change_gc(gc, &x11rb::protocol::xproto::ChangeGCAux::new().foreground(0x0000ff))?;
    c.poly_fill_rectangle(src, gc, &[Rectangle { x: 2, y: 0, width: 2, height: 4 }])?;

    let dst = c.generate_id()?;
    c.create_pixmap(24, dst, root, 8, 8)?;
    let (ps, pd) = (c.generate_id()?, c.generate_id()?);
    c.render_create_picture(ps, src, rgb, &Default::default())?;
    c.render_create_picture(pd, dst, rgb, &Default::default())?;

    // twice the size through a transform (destination to source: halve)
    let half = 1 << 15;
    let one = 1 << 16;
    let t = render::Transform { matrix11: half, matrix12: 0, matrix13: 0, matrix21: 0, matrix22: half, matrix23: 0, matrix31: 0, matrix32: 0, matrix33: one };
    c.render_set_picture_transform(ps, t)?;
    c.render_composite(PictOp::SRC, ps, 0u32, pd, 0, 0, 0, 0, 0, 0, 8, 8)?;

    // half-transparent white over the left (red): pink
    let white = render::Color { red: 0xffff, green: 0xffff, blue: 0xffff, alpha: 0x8000 };
    c.render_fill_rectangles(PictOp::OVER, pd, white, &[Rectangle { x: 0, y: 6, width: 2, height: 2 }])?;

    // a glyph (2x2, full coverage) in green at (5, 5)
    let gs = c.generate_id()?;
    c.render_create_glyph_set(gs, a8)?;
    let info = render::Glyphinfo { width: 2, height: 2, x: 0, y: 0, x_off: 2, y_off: 0 };
    c.render_add_glyphs(gs, &[7], &[info], &[255, 255, 0, 0, 255, 255, 0, 0])?;
    let green = c.generate_id()?;
    c.render_create_solid_fill(green, render::Color { red: 0, green: 0xffff, blue: 0, alpha: 0xffff })?;
    // one element: one glyph, moved to (5, 5)
    let cmds = [1u8, 0, 0, 0, 5, 0, 5, 0, 7, 0, 0, 0];
    c.render_composite_glyphs8(PictOp::OVER, green, pd, 0u32, gs, 0, 0, &cmds)?;

    let img = c.get_image(ImageFormat::Z_PIXMAP, dst, 0, 0, 8, 8, !0)?.reply()?;
    let px = |x: usize, y: usize| {
        let o = (y * 8 + x) * 4;
        (img.data[o + 2], img.data[o + 1], img.data[o])
    };
    check("transform: scaled left half is red", px(1, 1), (255, 0, 0));
    check("transform: scaled right half is blue", px(6, 1), (0, 0, 255));
    check("over: half white on red is pink", px(1, 7), (255, 128, 128));
    check("glyph: green where the glyph is", px(5, 5), (0, 255, 0));
    check("glyph: untouched next to it", px(4, 4), (0, 0, 255));
    // an ARGB window (the depth-32 visual), half-transparent red through RENDER
    let screen = &c.setup().roots[screen];
    let argb = screen.allowed_depths.iter().find(|d| d.depth == 32).and_then(|d| d.visuals.first()).map(|v| v.visual_id);
    match argb {
        None => {
            println!("FAIL argb: no depth-32 visual");
            failed += 1;
        }
        Some(visual) => {
            let cmap = c.generate_id()?;
            c.create_colormap(x11rb::protocol::xproto::ColormapAlloc::NONE, cmap, root, visual)?;
            let w = c.generate_id()?;
            let aux = x11rb::protocol::xproto::CreateWindowAux::new().colormap(cmap).border_pixel(0);
            c.create_window(32, w, root, 0, 0, 16, 16, 0, x11rb::protocol::xproto::WindowClass::INPUT_OUTPUT, visual, &aux)?;
            c.map_window(w)?;
            c.flush()?;
            // let loom show it before drawing (an unshown window draws nothing)
            std::thread::sleep(std::time::Duration::from_millis(600));
            let argb_fmt = by_depth(32);
            let pw = c.generate_id()?;
            c.render_create_picture(pw, w, argb_fmt, &Default::default())?;
            let red = render::Color { red: 0xffff, green: 0, blue: 0, alpha: 0x8000 };
            c.render_fill_rectangles(PictOp::SRC, pw, red, &[Rectangle { x: 0, y: 0, width: 16, height: 16 }])?;
            let img = c.get_image(ImageFormat::Z_PIXMAP, w, 0, 0, 1, 1, !0)?.reply()?;
            let (b, g, r, a) = (img.data[0], img.data[1], img.data[2], img.data[3]);
            println!("{} argb: depth {} pixel a={a} r={r} g={g} b={b}, want a=128 r=255", if img.depth == 32 && (a as i32 - 128).abs() <= 2 && r >= 250 { "PASS" } else { "FAIL" }, img.depth);
            if !(img.depth == 32 && (a as i32 - 128).abs() <= 2 && r >= 250) {
                failed += 1;
            }
        }
    }
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}
