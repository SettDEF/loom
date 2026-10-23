//! xdnd_target: an X window that takes drops (XDND 5): prints `enter TYPES`, `position X,Y`,
//! `dropped DATA` (as text/uri-list) or `leave`, then exits. See dnd_source for the test.

use x11rb::{
    connection::Connection,
    protocol::{
        xproto::{AtomEnum, ClientMessageEvent, ConnectionExt, CreateWindowAux, EventMask, PropMode, WindowClass},
        Event,
    },
    wrapper::ConnectionExt as _,
    CURRENT_TIME,
};

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        XdndAware, XdndSelection, XdndEnter, XdndPosition, XdndStatus, XdndLeave, XdndDrop,
        XdndFinished, XdndActionCopy, XDND_DATA, WM_NAME,
        URI: b"text/uri-list",
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    let a = Atoms::new(&conn)?.reply()?;
    let win = conn.generate_id()?;
    conn.create_window(0, win, root, 0, 0, 400, 300, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new().background_pixel(0x3060e0).event_mask(EventMask::PROPERTY_CHANGE))?;
    conn.change_property32(PropMode::REPLACE, win, a.XdndAware, AtomEnum::ATOM, &[5])?;
    conn.change_property8(PropMode::REPLACE, win, a.WM_NAME, AtomEnum::STRING, b"xdnd_target")?;
    conn.map_window(win)?;
    conn.flush()?;
    println!("shown");
    let name = |atom: u32| conn.get_atom_name(atom).ok().and_then(|c| c.reply().ok()).map(|r| String::from_utf8_lossy(&r.name).into_owned()).unwrap_or_default();
    let mut source = 0;
    let mut positions = 0;
    loop {
        match conn.wait_for_event()? {
            Event::ClientMessage(m) if m.type_ == a.XdndEnter => {
                let d = m.data.as_data32();
                source = d[0];
                let types: Vec<String> = d[2..].iter().filter(|t| **t != 0).map(|t| name(*t)).collect();
                println!("enter v{} {}", d[1] >> 24, types.join(" "));
            }
            Event::ClientMessage(m) if m.type_ == a.XdndPosition => {
                let d = m.data.as_data32();
                positions += 1;
                if positions == 1 {
                    println!("position {},{}", d[2] >> 16, d[2] & 0xffff);
                }
                let ev = ClientMessageEvent::new(32, source, a.XdndStatus, [win, 1, 0, 0, a.XdndActionCopy]);
                conn.send_event(false, source, EventMask::NO_EVENT, ev)?;
                conn.flush()?;
            }
            Event::ClientMessage(m) if m.type_ == a.XdndLeave => {
                println!("leave");
                return Ok(());
            }
            Event::ClientMessage(m) if m.type_ == a.XdndDrop => {
                println!("drop after {positions} positions");
                conn.convert_selection(win, a.XdndSelection, a.URI, a.XDND_DATA, CURRENT_TIME)?;
                conn.flush()?;
            }
            Event::SelectionNotify(n) => {
                let data = conn.get_property(true, win, a.XDND_DATA, AtomEnum::ANY, 0, 1 << 20)?.reply()?;
                println!("dropped {:?} (property {})", String::from_utf8_lossy(&data.value), n.property != 0);
                let ev = ClientMessageEvent::new(32, source, a.XdndFinished, [win, 1, a.XdndActionCopy, 0, 0]);
                conn.send_event(false, source, EventMask::NO_EVENT, ev)?;
                conn.flush()?;
                std::thread::sleep(std::time::Duration::from_millis(300));
                return Ok(());
            }
            _ => {}
        }
    }
}
