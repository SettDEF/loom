//! Prints a server's GLX visual and framebuffer configs (to compare loom-x with Xwayland). Usage: DISPLAY=:N cargo run --example glx_configs
use x11rb::{connection::Connection, protocol::glx::ConnectionExt};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (c, screen) = x11rb::connect(None)?;
    let v = c.glx_get_visual_configs(screen as u32)?.reply()?;
    println!("visual configs: {} x {} values", v.num_visuals, v.num_properties);
    for (i, chunk) in v.property_list.chunks(v.num_properties as usize).enumerate().take(3) {
        println!("  visual {i}: {:x?}", chunk);
    }
    let f = c.glx_get_fb_configs(screen as u32)?.reply()?;
    println!("fb configs: {} x {} pairs", f.num_fb_configs, f.num_properties);
    for (i, chunk) in f.property_list.chunks(f.num_properties as usize * 2).enumerate().take(2) {
        let pairs: Vec<String> = chunk.chunks(2).map(|p| format!("{:#x}={:#x}", p[0], p[1])).collect();
        println!("  fbconfig {i}: {}", pairs.join(" "));
    }
    Ok(())
}
