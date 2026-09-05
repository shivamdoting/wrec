use crate::backend;
use domain::{CaptureSourceKind, CaptureTarget, Result};
use x11rb::{
    connection::Connection,
    protocol::xproto::{AtomEnum, ConnectionExt, MapState},
};

pub(crate) fn initialize() -> Result<()> {
    static XLIB: std::sync::OnceLock<std::result::Result<x11_dl::xlib::Xlib, String>> =
        std::sync::OnceLock::new();
    static INITIALIZED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let xlib = XLIB
        .get_or_init(|| x11_dl::xlib::Xlib::open().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(backend)?;
    // Must run before GStreamer opens any Xlib display. Discovery uses x11rb,
    // which talks to the server directly and does not initialize Xlib.
    if !INITIALIZED.get_or_init(|| unsafe { (xlib.XInitThreads)() } != 0) {
        return Err(backend("XInitThreads failed"));
    }
    Ok(())
}

pub(crate) fn list_targets() -> Result<Vec<CaptureTarget>> {
    let (connection, default_screen) = x11rb::connect(None).map_err(backend)?;
    let atoms = |name: &[u8]| -> Result<u32> {
        Ok(connection
            .intern_atom(false, name)
            .map_err(backend)?
            .reply()
            .map_err(backend)?
            .atom)
    };
    let clients = atoms(b"_NET_CLIENT_LIST_STACKING")?;
    let utf8_name = atoms(b"_NET_WM_NAME")?;
    let mut targets = Vec::new();
    for (index, screen) in connection.setup().roots.iter().enumerate() {
        targets.push(CaptureTarget {
            id: if index == default_screen {
                0
            } else {
                screen.root.into()
            },
            kind: CaptureSourceKind::Display,
            name: format!(
                "X11 display {index} ({}×{})",
                screen.width_in_pixels, screen.height_in_pixels
            ),
        });
        let reply = connection
            .get_property(false, screen.root, clients, AtomEnum::WINDOW, 0, 4096)
            .map_err(backend)?
            .reply()
            .map_err(backend)?;
        let windows: Vec<u32> = reply.value32().map(|v| v.collect()).unwrap_or_else(|| {
            connection
                .query_tree(screen.root)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| r.children)
                .unwrap_or_default()
        });
        for window in windows {
            let visible = connection
                .get_window_attributes(window)
                .ok()
                .and_then(|c| c.reply().ok())
                .is_some_and(|a| a.map_state == MapState::VIEWABLE);
            if !visible {
                continue;
            }
            let property = |atom: u32| {
                connection
                    .get_property(false, window, atom, AtomEnum::ANY, 0, 1024)
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .map(|r| {
                        String::from_utf8_lossy(&r.value)
                            .trim_end_matches('\0')
                            .to_string()
                    })
            };
            let name = property(utf8_name)
                .filter(|s| !s.is_empty())
                .or_else(|| property(AtomEnum::WM_NAME.into()))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("X11 window {window}"));
            targets.push(CaptureTarget {
                id: window.into(),
                name,
                kind: CaptureSourceKind::Window,
            });
        }
    }
    Ok(targets)
}

pub(crate) fn source(target: &CaptureTarget) -> Result<(String, u64)> {
    let display = std::env::var("DISPLAY").map_err(backend)?;
    if !list_targets()?
        .iter()
        .any(|t| t.id == target.id && t.kind == target.kind)
    {
        return Err(backend(
            "The selected X11 display or window is no longer available.",
        ));
    }
    Ok((display, target.id))
}
