//! Hyprland's input settings on libinput devices (`input:`, `input:touchpad:`, `device { name = … }`
//! and `keyword device:NAME:key`), and the devices for `hyprctl devices`.

use std::collections::HashMap;

use smithay::reexports::input::{AccelProfile, ClickMethod, Device, DeviceCapability, SendEventsMode};

/// An input device, its name as Hyprland writes it; a mouse or touchpad (pointer) has a scroll factor.
pub struct Dev {
    pub dev: Device,
    pub name: String,
    pub pointer: bool,
    pub touchpad: bool,
    pub scroll: f64,
}

#[derive(Default)]
pub struct Devices {
    pub all: Vec<Dev>,
}

/// Hyprland's name for a device: lower case, spaces as dashes.
pub fn hypr_name(n: &str) -> String {
    n.to_lowercase().replace(' ', "-")
}

/// One device's settings, later ones winning: `input:`, then (a touchpad) `input:touchpad:`, then its
/// own. `tap-to-click` and `tap_to_click` are one key.
pub fn settings(lines: &[(String, String)], name: &str, touchpad: bool) -> HashMap<String, String> {
    let norm = |k: &str| k.replace('-', "_");
    let (mut global, mut pad, mut own) = (HashMap::new(), HashMap::new(), HashMap::new());
    let mut block: Option<String> = None;
    for (k, v) in lines {
        let v = v.trim().to_string();
        if let Some(key) = k.strip_prefix("input:touchpad:") {
            pad.insert(norm(key), v);
        } else if let Some(key) = k.strip_prefix("input:").filter(|k| !k.contains(':')) {
            global.insert(norm(key), v);
        } else if k == "device:name" {
            block = Some(v);
        } else if let Some(rest) = k.strip_prefix("device:") {
            match rest.split_once(':') {
                Some((n, key)) if n == name => drop(own.insert(norm(key), v)),
                Some(_) => {}
                None if block.as_deref() == Some(name) => drop(own.insert(norm(rest), v)),
                None => {}
            }
        }
    }
    // a touchpad's scrolling is its own (input:natural_scroll is the mouse's)
    if touchpad {
        global.remove("natural_scroll");
        global.remove("scroll_factor");
        global.extend(pad);
    }
    global.extend(own);
    global
}

/// The settings put on the device; its scroll factor.
pub fn apply(dev: &mut Device, s: &HashMap<String, String>) -> f64 {
    let on = |k: &str, d: bool| s.get(k).map_or(d, |v| !matches!(v.as_str(), "0" | "false" | "no" | "off"));
    let num = |k: &str| s.get(k).and_then(|v| v.parse::<f64>().ok());
    if dev.config_tap_finger_count() > 0 {
        let _ = dev.config_tap_set_enabled(on("tap_to_click", true));
        let _ = dev.config_tap_set_drag_enabled(on("tap_and_drag", true));
        let _ = dev.config_tap_set_drag_lock_enabled(on("drag_lock", false));
    }
    if dev.config_dwt_is_available() {
        let _ = dev.config_dwt_set_enabled(on("disable_while_typing", true));
    }
    if dev.config_scroll_has_natural_scroll() {
        let _ = dev.config_scroll_set_natural_scroll_enabled(on("natural_scroll", false));
    }
    if dev.config_left_handed_is_available() {
        let _ = dev.config_left_handed_set(on("left_handed", false));
    }
    if dev.config_middle_emulation_is_available() {
        let _ = dev.config_middle_emulation_set_enabled(on("middle_button_emulation", false));
    }
    if dev.config_accel_is_available() {
        let _ = dev.config_accel_set_speed(num("sensitivity").unwrap_or(0.0).clamp(-1.0, 1.0));
        match s.get("accel_profile").map(String::as_str) {
            Some("flat") => drop(dev.config_accel_set_profile(AccelProfile::Flat)),
            Some("adaptive") => drop(dev.config_accel_set_profile(AccelProfile::Adaptive)),
            _ => {}
        }
    }
    let methods = dev.config_click_methods();
    let method = if on("clickfinger_behavior", false) { ClickMethod::Clickfinger } else { ClickMethod::ButtonAreas };
    if methods.contains(&method) {
        let _ = dev.config_click_set_method(method);
    }
    let _ = dev.config_send_events_set_mode(if on("enabled", true) { SendEventsMode::ENABLED } else { SendEventsMode::DISABLED });
    num("scroll_factor").unwrap_or(1.0)
}

impl Devices {
    /// A new device, its settings put on it.
    pub fn added(&mut self, mut dev: Device, lines: &[(String, String)]) {
        let name = hypr_name(&dev.name());
        let touchpad = dev.config_tap_finger_count() > 0;
        let scroll = apply(&mut dev, &settings(lines, &name, touchpad));
        let pointer = dev.has_capability(DeviceCapability::Pointer);
        self.all.push(Dev { dev, name, pointer, touchpad, scroll });
    }

    pub fn removed(&mut self, dev: &Device) {
        self.all.retain(|p| &p.dev != dev);
    }

    /// The settings again (a `keyword` changed them).
    pub fn reapply(&mut self, lines: &[(String, String)]) {
        for p in &mut self.all {
            p.scroll = apply(&mut p.dev, &settings(lines, &p.name, p.touchpad));
        }
    }

    /// The scroll factor of the device a scroll came from.
    pub fn scroll_of(&self, dev: &Device) -> f64 {
        self.all.iter().find(|p| &p.dev == dev).map_or(1.0, |p| p.scroll)
    }
}

#[cfg(test)]
mod tests {
    fn l(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn device_blocks_win_and_touchpads_scroll_their_own_way() {
        let lines = l(&[
            ("input:sensitivity", "0.2"),
            ("input:natural_scroll", "true"),
            ("input:scroll_factor", "2"),
            ("input:touchpad:scroll_factor", "1.5"),
            ("input:touchpad:tap-to-click", "false"),
            ("device:name", "pad"),
            ("device:sensitivity", "-0.64"),
            ("device:name", "other"),
            ("device:enabled", "false"),
            ("device:pad:tap_to_click", "true"),
        ]);
        let pad = super::settings(&lines, "pad", true);
        assert_eq!(pad.get("sensitivity").map(String::as_str), Some("-0.64"));
        assert_eq!(pad.get("scroll_factor").map(String::as_str), Some("1.5"));
        assert_eq!(pad.get("natural_scroll"), None);
        assert_eq!(pad.get("tap_to_click").map(String::as_str), Some("true"));
        assert_eq!(pad.get("enabled"), None);
        let mouse = super::settings(&lines, "mouse", false);
        assert_eq!(mouse.get("scroll_factor").map(String::as_str), Some("2"));
        assert_eq!(mouse.get("natural_scroll").map(String::as_str), Some("true"));
        assert_eq!(super::settings(&lines, "other", false).get("enabled").map(String::as_str), Some("false"));
        assert_eq!(super::hypr_name("ASUSTeK Computer Inc. GZ302EA-Keyboard Touchpad"), "asustek-computer-inc.-gz302ea-keyboard-touchpad");
    }
}
