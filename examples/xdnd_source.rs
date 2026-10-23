//! xdnd_source: an X window that drags `file:///tmp/loom-xdnd.wav` (XDND 5) when pressed, as a
//! toolkit does: the pointer grabbed, Enter/Position to the XdndAware window under it, Drop on
//! release. Prints `enter`, `status accept`, `drop`, `sent`, `finished ok=…`. With dnd_target:
//! `loom --headless --drag 400,150,400,460 -- sh -c 'xdnd_source & sleep 1.5; dnd_target'`.

use x11rb::{
    connection::Connection,
    protocol::{
        xproto::{AtomEnum, ClientMessageEvent, ConnectionExt, CreateWindowAux, EventMask, GrabMode, PropMode, SelectionNotifyEvent, WindowClass, SELECTION_NOTIFY_EVENT},
        Event,
    },
    wrapper::ConnectionExt as _,
    CURRENT_TIME, NONE,
};

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        XdndAware, XdndSelection, XdndEnter, XdndPosition, XdndStatus, XdndLeave, XdndDrop,
        XdndFinished, XdndActionCopy, WM_NAME,
        URI: b"text/uri-list",
    }
}

const DATA: &[u8] = b"file:///tmp/loom-xdnd.wav\r\n";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    let a = Atoms::new(&conn)?.reply()?;
    let win = conn.generate_id()?;
    let mask = EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION;
    conn.create_window(0, win, root, 0, 0, 400, 300, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new().background_pixel(0x30a060).event_mask(mask))?;
    conn.change_property8(PropMode::REPLACE, win, a.WM_NAME, AtomEnum::STRING, b"xdnd_source")?;
    conn.map_window(win)?;
    conn.flush()?;
    println!("shown");
    let (mut dragging, mut target, mut accepted, mut waiting) = (false, NONE, false, false);
    let send = |to: u32, kind: u32, data: [u32; 5]| -> Result<(), Box<dyn std::error::Error>> {
        conn.send_event(false, to, EventMask::NO_EVENT, ClientMessageEvent::new(32, to, kind, data))?;
        conn.flush()?;
        Ok(())
    };
    loop {
        match conn.wait_for_event()? {
            Event::ButtonPress(b) => {
                conn.set_selection_owner(win, a.XdndSelection, b.time)?;
                conn.grab_pointer(false, win, EventMask::POINTER_MOTION | EventMask::BUTTON_RELEASE, GrabMode::ASYNC, GrabMode::ASYNC, NONE, NONE, b.time)?;
                conn.flush()?;
                dragging = true;
                println!("drag started");
            }
            Event::MotionNotify(m) if dragging => {
                // the top window under the pointer, if it takes drops
                let child = conn.query_pointer(root)?.reply()?.child;
                let aware = child != NONE && child != win && conn.get_property(false, child, a.XdndAware, AtomEnum::ATOM, 0, 1)?.reply()?.value32().and_then(|mut v| v.next()).is_some();
                let now = if aware { child } else { NONE };
                if now != target {
                    if target != NONE {
                        send(target, a.XdndLeave, [win, 0, 0, 0, 0])?;
                    }
                    if now != NONE {
                        send(now, a.XdndEnter, [win, 5 << 24, a.URI, 0, 0])?;
                        println!("enter");
                    }
                    (target, accepted, waiting) = (now, false, false);
                }
                if target != NONE && !waiting {
                    send(target, a.XdndPosition, [win, 0, ((m.root_x as u16 as u32) << 16) | m.root_y as u16 as u32, m.time, a.XdndActionCopy])?;
                    waiting = true;
                }
            }
            Event::ClientMessage(c) if c.type_ == a.XdndStatus => {
                let was = accepted;
                accepted = c.data.as_data32()[1] & 1 == 1;
                waiting = false;
                if accepted && !was {
                    println!("status accept");
                }
            }
            Event::ButtonRelease(b) if dragging => {
                dragging = false;
                conn.ungrab_pointer(b.time)?;
                if target != NONE && accepted {
                    send(target, a.XdndDrop, [win, 0, b.time, 0, 0])?;
                    println!("drop");
                } else {
                    if target != NONE {
                        send(target, a.XdndLeave, [win, 0, 0, 0, 0])?;
                    }
                    println!("released, not taken");
                    return Ok(());
                }
            }
            Event::SelectionRequest(r) if r.selection == a.XdndSelection => {
                let property = if r.target == a.URI {
                    conn.change_property8(PropMode::REPLACE, r.requestor, r.property, a.URI, DATA)?;
                    println!("sent");
                    r.property
                } else {
                    NONE
                };
                let ev = SelectionNotifyEvent { response_type: SELECTION_NOTIFY_EVENT, sequence: 0, time: r.time, requestor: r.requestor, selection: r.selection, target: r.target, property };
                conn.send_event(false, r.requestor, EventMask::NO_EVENT, ev)?;
                conn.flush()?;
            }
            Event::ClientMessage(c) if c.type_ == a.XdndFinished => {
                println!("finished ok={}", c.data.as_data32()[1] & 1);
                let _ = CURRENT_TIME;
                return Ok(());
            }
            _ => {}
        }
    }
}
