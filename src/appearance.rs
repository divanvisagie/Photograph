//! Follows the desktop's light/dark preference on Linux (see ADR-0021).
//!
//! winit never reports a system theme on Linux, so egui falls back to dark
//! whatever the desktop says. This reads the `color-scheme` setting from the
//! XDG desktop portal (what GNOME's and KDE's dark-style switches set) and
//! listens for changes on its own thread, setting egui's fallback theme to
//! match. Where a platform does report its theme, egui uses that and the
//! fallback is ignored. With no session bus or portal, nothing changes.

use egui::Theme;
use zbus::zvariant::OwnedValue;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS_IFACE: &str = "org.freedesktop.portal.Settings";
const NAMESPACE: &str = "org.freedesktop.appearance";
const KEY: &str = "color-scheme";

/// Starts watching the desktop's colour scheme.
pub fn watch(ctx: &egui::Context) {
    let ctx = ctx.clone();
    let _ = std::thread::Builder::new()
        .name("appearance".into())
        .spawn(move || {
            if let Err(e) = run(&ctx) {
                eprintln!("photograph: not following the desktop colour scheme: {e}");
            }
        });
}

fn run(ctx: &egui::Context) -> zbus::Result<()> {
    let conn = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, PORTAL, PORTAL_PATH, SETTINGS_IFACE)?;
    // Subscribe before reading, so a change in between isn't missed.
    let changes = proxy.receive_signal("SettingChanged")?;
    set(ctx, read(&proxy)?);
    for signal in changes {
        let Ok((namespace, key, value)) = signal.body().deserialize::<(String, String, OwnedValue)>() else {
            continue;
        };
        if namespace == NAMESPACE && key == KEY {
            set(ctx, scheme(&value));
        }
    }
    Ok(())
}

fn read(proxy: &zbus::blocking::Proxy) -> zbus::Result<Option<Theme>> {
    let value: OwnedValue = match proxy.call("ReadOne", &(NAMESPACE, KEY)) {
        Ok(value) => value,
        // Portals older than version 2 only have the deprecated Read, which
        // wraps the value in a second variant; `scheme` unwraps it.
        Err(_) => proxy.call("Read", &(NAMESPACE, KEY))?,
    };
    Ok(scheme(&value))
}

/// The portal's `color-scheme`: 1 prefers dark, 2 prefers light, and 0 (no
/// preference) means the default, which for GNOME and egui alike is light.
fn scheme(value: &OwnedValue) -> Option<Theme> {
    let mut value: &zbus::zvariant::Value = value;
    while let zbus::zvariant::Value::Value(inner) = value {
        value = inner;
    }
    match u32::try_from(value).ok()? {
        1 => Some(Theme::Dark),
        0 | 2 => Some(Theme::Light),
        _ => None,
    }
}

fn set(ctx: &egui::Context, theme: Option<Theme>) {
    let Some(theme) = theme else {
        return;
    };
    ctx.options_mut(|o| o.fallback_theme = theme);
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    fn owned(v: Value) -> OwnedValue {
        v.try_into_owned().unwrap()
    }

    #[test]
    fn maps_portal_color_schemes() {
        assert_eq!(scheme(&owned(Value::U32(1))), Some(Theme::Dark));
        assert_eq!(scheme(&owned(Value::U32(2))), Some(Theme::Light));
        assert_eq!(scheme(&owned(Value::U32(0))), Some(Theme::Light));
        assert_eq!(scheme(&owned(Value::U32(7))), None);
        assert_eq!(scheme(&owned(Value::from("dark"))), None);
    }

    #[test]
    fn unwraps_nested_variants_from_the_old_read_call() {
        let nested = Value::Value(Box::new(Value::Value(Box::new(Value::U32(1)))));
        assert_eq!(scheme(&owned(nested)), Some(Theme::Dark));
    }
}
