//! KDE's output protocols, so System Settings › Display and `kscreen-doctor` see and change loom's
//! screens: kde_output_device_registry_v2 lists each screen (kde_output_device_v2: place, size,
//! modes, scale, adaptive sync) and kde_output_management_v2 takes a change and applies it
//! (position, scale, mode, adaptive sync policy). Rotation and turning a screen off are refused
//! with a reason (loom does not draw rotated screens yet).

use std::sync::Mutex;

use smithay::{
    output::{Mode, Scale},
    reexports::wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum},
};

macro_rules! protocol {
    ($name:ident, $file:literal $(, $dep:ident)?) => {
        pub mod $name {
            #![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables, non_upper_case_globals, non_snake_case, unused_imports, missing_docs, clippy::all)]
            use wayland_server;
            use wayland_server::protocol::*;
            $(use super::$dep::*;)?
            pub mod __interfaces {
                use wayland_server::protocol::__interfaces::*;
                use wayland_server::backend as wayland_backend;
                $(use super::super::$dep::__interfaces::*;)?
                wayland_scanner::generate_interfaces!($file);
            }
            use self::__interfaces::*;
            wayland_scanner::generate_server_code!($file);
        }
    };
}
protocol!(dev, "protocols/kde-output-device-v2.xml");
protocol!(mgmt, "protocols/kde-output-management-v2.xml", dev);
protocol!(order, "protocols/kde-output-order-v1.xml");
protocol!(primary, "protocols/kde-primary-output-v1.xml");

use dev::{
    kde_output_device_mode_v2::{self as mode_v2, KdeOutputDeviceModeV2},
    kde_output_device_registry_v2::{self as reg_v2, KdeOutputDeviceRegistryV2},
    kde_output_device_v2::{self as dev_v2, KdeOutputDeviceV2},
};
use mgmt::{
    kde_mode_list_v2::{self as list_v2, KdeModeListV2},
    kde_output_configuration_v2::{self as cfg_v2, KdeOutputConfigurationV2},
    kde_output_management_v2::{self as mgmt_v2, KdeOutputManagementV2},
};

use order::kde_output_order_v1::{self as order_v1, KdeOutputOrderV1};
use primary::kde_primary_output_v1::{self as primary_v1, KdePrimaryOutputV1};

use crate::state::Loom;

/// One screen as one client sees it: its device object and the mode objects sent with it.
pub struct Device {
    pub dev: KdeOutputDeviceV2,
    pub output: String,
    pub modes: Vec<(KdeOutputDeviceModeV2, Mode)>,
}

#[derive(Default)]
pub struct State {
    pub registries: Vec<KdeOutputDeviceRegistryV2>,
    pub devices: Vec<Device>,
    /// the screens' order and the primary one, to those who asked (Plasma's panels, kscreen)
    pub orders: Vec<KdeOutputOrderV1>,
    pub primaries: Vec<KdePrimaryOutputV1>,
}

/// A change asked for in a configuration, applied all together.
#[derive(Clone, Debug)]
enum Change {
    Enable(String, bool),
    Mode(String, Mode),
    Transform(String, i32),
    Position(String, i32, i32),
    Scale(String, f64),
    Vrr(String, u32),
}

pub fn init(dh: &DisplayHandle) {
    dh.create_global::<Loom, KdeOutputDeviceRegistryV2, ()>(26, ());
    dh.create_global::<Loom, KdeOutputManagementV2, ()>(22, ());
    dh.create_global::<Loom, KdeOutputOrderV1, ()>(1, ());
    dh.create_global::<Loom, KdePrimaryOutputV1, ()>(2, ());
}

impl Loom {
    /// What one device says about its screen (all of it the first time; `modes` only then).
    fn kde_describe(&self, d: &mut Device, first: bool, dh: &DisplayHandle) {
        let Some(o) = self.outputs.iter().find(|o| o.name() == d.output).cloned() else { return };
        let props = o.physical_properties();
        let at = self.space.output_geometry(&o).map(|g| g.loc).unwrap_or_default();
        d.dev.geometry(at.x, at.y, props.size.w, props.size.h, 0, props.make.clone(), props.model.clone(), 0);
        let current = o.current_mode();
        if first {
            let mut modes = o.modes();
            if let Some(c) = current {
                if !modes.contains(&c) {
                    modes.push(c);
                }
            }
            let preferred = o.preferred_mode();
            let Some(client) = d.dev.client() else { return };
            for m in modes {
                let Ok(r) = client.create_resource::<KdeOutputDeviceModeV2, _, Loom>(dh, d.dev.version(), ()) else { continue };
                d.dev.mode(&r);
                r.size(m.size.w, m.size.h);
                r.refresh(m.refresh);
                if Some(m) == preferred {
                    r.preferred();
                }
                d.modes.push((r, m));
            }
        }
        if let Some((r, _)) = d.modes.iter().find(|(_, m)| Some(*m) == current) {
            d.dev.current_mode(r);
        }
        d.dev.scale(o.current_scale().fractional_scale());
        if first {
            d.dev.edid(String::new());
            d.dev.uuid(format!("loom-{}", d.output));
            d.dev.serial_number(String::new());
            d.dev.eisa_id(String::new());
            if d.dev.version() >= 2 {
                d.dev.name(d.output.clone());
            }
        }
        d.dev.enabled(1);
        let (can, policy) = self.kde_vrr(&d.output);
        d.dev.capabilities(if can { dev_v2::Capability::Vrr } else { dev_v2::Capability::empty() });
        d.dev.overscan(0);
        d.dev.vrr_policy(dev_v2::VrrPolicy::try_from(policy).unwrap_or(dev_v2::VrrPolicy::Never));
        d.dev.rgb_range(dev_v2::RgbRange::Automatic);
        // the screens in order, the first one primary
        if d.dev.version() >= 18 {
            let i = self.outputs.iter().position(|o| o.name() == d.output).unwrap_or(0);
            d.dev.priority(i as u32 + 1);
        }
        d.dev.done();
    }

    /// Whether a screen can do adaptive sync, and its policy (KDE's never/always/automatic are
    /// loom's vrr 0/1/2).
    fn kde_vrr(&self, name: &str) -> (bool, u32) {
        self.tty.as_ref().and_then(|t| t.screens.iter().find(|s| s.output.name() == name)).map(|s| (s.vrr_can, s.vrr as u32)).unwrap_or((false, 0))
    }

    /// A registry bound, or a screen added: each registry gets a device for each screen it lacks.
    pub fn kde_outputs_changed(&mut self) {
        let dh = self.dh.clone();
        let mut st = std::mem::take(&mut self.kde);
        // screens gone: their devices say so
        st.devices.retain(|d| {
            let keep = self.outputs.iter().any(|o| o.name() == d.output);
            if !keep && d.dev.version() >= 21 {
                d.dev.removed();
            }
            keep
        });
        for reg in st.registries.clone() {
            let Some(client) = reg.client() else { continue };
            for o in self.outputs.clone() {
                if st.devices.iter().any(|d| d.output == o.name() && d.dev.client().as_ref() == Some(&client)) {
                    continue;
                }
                let Ok(dev) = client.create_resource::<KdeOutputDeviceV2, _, Loom>(&dh, reg.version(), o.name()) else { continue };
                reg.output(&dev);
                let mut d = Device { dev, output: o.name(), modes: Vec::new() };
                self.kde_describe(&mut d, true, &dh);
                st.devices.push(d);
            }
        }
        self.kde = st;
        self.kde_order_changed();
    }

    /// The screens in order (the first is primary), to everyone who listens.
    fn kde_order_changed(&self) {
        let names: Vec<String> = self.outputs.iter().map(|o| o.name()).collect();
        for o in &self.kde.orders {
            for n in &names {
                o.output(n.clone());
            }
            o.done();
        }
        if let Some(first) = names.first() {
            for p in &self.kde.primaries {
                p.primary_output(first.clone());
            }
        }
    }

    /// After a change: every device says how its screen is now.
    fn kde_refresh(&mut self) {
        let dh = self.dh.clone();
        let mut st = std::mem::take(&mut self.kde);
        for d in &mut st.devices {
            self.kde_describe(d, false, &dh);
        }
        self.kde = st;
    }

    fn kde_apply(&mut self, changes: Vec<Change>) -> Result<(), String> {
        for c in &changes {
            match c {
                Change::Enable(n, false) => return Err(format!("loom cannot turn {n} off yet")),
                Change::Transform(n, t) if *t != 0 => return Err(format!("loom does not rotate screens yet ({n})")),
                _ => {}
            }
        }
        let out = |s: &Loom, n: &str| s.outputs.iter().find(|o| o.name() == n).cloned();
        for c in changes {
            match c {
                Change::Position(n, x, y) => {
                    let Some(o) = out(self, &n) else { continue };
                    self.space.map_output(&o, (x, y));
                    o.change_current_state(None, None, None, Some((x, y).into()));
                }
                Change::Scale(n, s) => {
                    let Some(o) = out(self, &n) else { continue };
                    let scale = if (s - s.round()).abs() < 1e-6 { Scale::Integer(s.round() as i32) } else { Scale::Fractional(s) };
                    o.change_current_state(None, None, Some(scale), None);
                }
                Change::Mode(n, m) => self.set_output_mode(&n, m)?,
                Change::Vrr(n, p) => {
                    if let Some(s) = self.tty.as_mut().and_then(|t| t.screens.iter_mut().find(|s| s.output.name() == n)) {
                        s.vrr = p.min(2) as u8;
                    }
                }
                Change::Enable(..) | Change::Transform(..) => {}
            }
        }
        self.x_layout_apply();
        self.relayout();
        self.dirty = true;
        Ok(())
    }

    /// A screen's mode changed: on the TTY its CRTC is set to it; nested and headless only say it.
    pub fn set_output_mode(&mut self, name: &str, m: Mode) -> Result<(), String> {
        let Some(o) = self.outputs.iter().find(|o| o.name() == name).cloned() else { return Err(format!("no screen {name}")) };
        if let Some(t) = self.tty.as_mut() {
            if let Some(s) = t.screens.iter_mut().find(|s| s.output.name() == name) {
                let drm = s.modes.iter().find(|d| Mode::from(**d) == m).copied().ok_or_else(|| format!("{name} has no mode {}x{}@{}", m.size.w, m.size.h, m.refresh))?;
                s.compositor.use_mode(drm).map_err(|e| format!("{name}: {e:?}"))?;
                s.period = std::time::Duration::from_micros(1_000_000_000 / m.refresh.max(1000) as u64);
                s.needs = true;
            }
        }
        o.change_current_state(Some(m), None, None, None);
        Ok(())
    }
}

impl GlobalDispatch<KdeOutputDeviceRegistryV2, ()> for Loom {
    fn bind(state: &mut Self, _: &DisplayHandle, _: &Client, resource: New<KdeOutputDeviceRegistryV2>, _: &(), data_init: &mut DataInit<'_, Self>) {
        let reg = data_init.init(resource, ());
        state.kde.registries.push(reg);
        state.kde_outputs_changed();
    }
}

impl Dispatch<KdeOutputDeviceRegistryV2, ()> for Loom {
    fn request(state: &mut Self, _: &Client, reg: &KdeOutputDeviceRegistryV2, request: reg_v2::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        // the only request: stop
        let reg_v2::Request::Stop = request;
        state.kde.registries.retain(|r| r != reg);
        reg.finished();
    }

    fn destroyed(state: &mut Self, _: smithay::reexports::wayland_server::backend::ClientId, reg: &KdeOutputDeviceRegistryV2, _: &()) {
        state.kde.registries.retain(|r| r != reg);
    }
}

impl Dispatch<KdeOutputDeviceV2, String> for Loom {
    fn request(_: &mut Self, _: &Client, _: &KdeOutputDeviceV2, _: dev_v2::Request, _: &String, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}

    fn destroyed(state: &mut Self, _: smithay::reexports::wayland_server::backend::ClientId, dev: &KdeOutputDeviceV2, _: &String) {
        state.kde.devices.retain(|d| &d.dev != dev);
    }
}

impl Dispatch<KdeOutputDeviceModeV2, ()> for Loom {
    fn request(_: &mut Self, _: &Client, _: &KdeOutputDeviceModeV2, _: mode_v2::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

impl GlobalDispatch<KdeOutputManagementV2, ()> for Loom {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<KdeOutputManagementV2>, _: &(), data_init: &mut DataInit<'_, Self>) {
        data_init.init(resource, ());
    }
}

impl Dispatch<KdeOutputManagementV2, ()> for Loom {
    fn request(_: &mut Self, _: &Client, _: &KdeOutputManagementV2, request: mgmt_v2::Request, _: &(), _: &DisplayHandle, data_init: &mut DataInit<'_, Self>) {
        match request {
            mgmt_v2::Request::CreateConfiguration { id } => {
                data_init.init(id, Mutex::new(Vec::<Change>::new()));
            }
            mgmt_v2::Request::CreateModeList { id } => {
                data_init.init(id, ());
            }
        }
    }
}

impl Dispatch<KdeModeListV2, ()> for Loom {
    fn request(_: &mut Self, _: &Client, _: &KdeModeListV2, _: list_v2::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

impl Dispatch<KdeOutputConfigurationV2, Mutex<Vec<Change>>> for Loom {
    fn request(state: &mut Self, _: &Client, cfg: &KdeOutputConfigurationV2, request: cfg_v2::Request, pending: &Mutex<Vec<Change>>, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        let name = |d: &KdeOutputDeviceV2| d.data::<String>().cloned().unwrap_or_default();
        let change = match request {
            cfg_v2::Request::Enable { outputdevice, enable } => Some(Change::Enable(name(&outputdevice), enable != 0)),
            cfg_v2::Request::Mode { outputdevice, mode } => {
                let m = state.kde.devices.iter().flat_map(|d| d.modes.iter()).find(|(r, _)| *r == mode).map(|(_, m)| *m);
                m.map(|m| Change::Mode(name(&outputdevice), m))
            }
            cfg_v2::Request::Transform { outputdevice, transform } => Some(Change::Transform(name(&outputdevice), transform)),
            cfg_v2::Request::Position { outputdevice, x, y } => Some(Change::Position(name(&outputdevice), x, y)),
            cfg_v2::Request::Scale { outputdevice, scale } => Some(Change::Scale(name(&outputdevice), scale)),
            cfg_v2::Request::SetVrrPolicy { outputdevice, policy } => Some(Change::Vrr(name(&outputdevice), match policy {
                WEnum::Value(p) => p as u32,
                WEnum::Unknown(p) => p,
            })),
            cfg_v2::Request::Apply => {
                let changes = std::mem::take(&mut *pending.lock().unwrap());
                match state.kde_apply(changes) {
                    Ok(()) => {
                        cfg.applied();
                        state.kde_refresh();
                    }
                    Err(e) => {
                        eprintln!("loom: screen change refused: {e}");
                        if cfg.version() >= 12 {
                            cfg.failure_reason(e);
                        }
                        cfg.failed();
                    }
                }
                None
            }
            _ => None,
        };
        if let Some(c) = change {
            pending.lock().unwrap().push(c);
        }
    }
}


impl GlobalDispatch<KdeOutputOrderV1, ()> for Loom {
    fn bind(state: &mut Self, _: &DisplayHandle, _: &Client, resource: New<KdeOutputOrderV1>, _: &(), data_init: &mut DataInit<'_, Self>) {
        let o = data_init.init(resource, ());
        for out in &state.outputs {
            o.output(out.name());
        }
        o.done();
        state.kde.orders.push(o);
    }
}

impl Dispatch<KdeOutputOrderV1, ()> for Loom {
    fn request(_: &mut Self, _: &Client, _: &KdeOutputOrderV1, _: order_v1::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}

    fn destroyed(state: &mut Self, _: smithay::reexports::wayland_server::backend::ClientId, o: &KdeOutputOrderV1, _: &()) {
        state.kde.orders.retain(|x| x != o);
    }
}

impl GlobalDispatch<KdePrimaryOutputV1, ()> for Loom {
    fn bind(state: &mut Self, _: &DisplayHandle, _: &Client, resource: New<KdePrimaryOutputV1>, _: &(), data_init: &mut DataInit<'_, Self>) {
        let p = data_init.init(resource, ());
        if let Some(first) = state.outputs.first() {
            p.primary_output(first.name());
        }
        state.kde.primaries.push(p);
    }
}

impl Dispatch<KdePrimaryOutputV1, ()> for Loom {
    fn request(_: &mut Self, _: &Client, _: &KdePrimaryOutputV1, _: primary_v1::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}

    fn destroyed(state: &mut Self, _: smithay::reexports::wayland_server::backend::ClientId, p: &KdePrimaryOutputV1, _: &()) {
        state.kde.primaries.retain(|x| x != p);
    }
}
