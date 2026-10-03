use crate::backend;
use domain::{CaptureSourceKind, CaptureTarget, Result};
use std::{
    cell::Cell,
    os::raw::{c_int, c_ulong},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
use x11_dl::xlib;
use x11rb::{
    connection::Connection,
    protocol::xproto::{AtomEnum, ConnectionExt, MapState},
    rust_connection::RustConnection,
};

type ErrorHandler = unsafe extern "C" fn(*mut xlib::Display, *mut xlib::XErrorEvent) -> c_int;

static PREVIOUS_HANDLER: OnceLock<Option<ErrorHandler>> = OnceLock::new();
static SOURCES: Mutex<Vec<Source>> = Mutex::new(Vec::new());

struct Source {
    xid: u32,
    damage: Option<(u8, u8)>,
    lost: Arc<AtomicBool>,
}

impl Source {
    fn claims(&self, error: &xlib::XErrorEvent) -> bool {
        if matches!(error.error_code, xlib::BadWindow | xlib::BadDrawable)
            && error.resourceid == c_ulong::from(self.xid)
        {
            self.lost.store(true, Ordering::SeqCst);
            return true;
        }
        self.lost.load(Ordering::SeqCst)
            && self.damage == Some((error.request_code, error.error_code))
    }
}

unsafe extern "C" fn handle_error(
    display: *mut xlib::Display,
    event: *mut xlib::XErrorEvent,
) -> c_int {
    if let Some(error) = event.as_ref() {
        if SOURCES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .any(|source| source.claims(error))
        {
            return 0;
        }
    }
    match PREVIOUS_HANDLER.get().copied().flatten() {
        Some(previous) => previous(display, event),
        None => 0,
    }
}

pub(crate) struct SourceWatch {
    xid: u32,
    lost: Arc<AtomicBool>,
    connection: RustConnection,
    checked: Cell<Option<Instant>>,
}

impl SourceWatch {
    pub(crate) fn new(xid: u64) -> Result<Self> {
        let xid = u32::try_from(xid).map_err(backend)?;
        let (connection, _) = x11rb::connect(None).map_err(backend)?;
        let damage = connection
            .query_extension(b"DAMAGE")
            .map_err(backend)?
            .reply()
            .map_err(backend)?;
        let lost = Arc::new(AtomicBool::new(false));
        SOURCES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(Source {
                xid,
                damage: damage
                    .present
                    .then_some((damage.major_opcode, damage.first_error)),
                lost: lost.clone(),
            });
        Ok(Self {
            xid,
            lost,
            connection,
            checked: Cell::new(None),
        })
    }

    pub(crate) fn lost(&self) -> bool {
        if !self.lost.load(Ordering::SeqCst)
            && self
                .checked
                .get()
                .map_or(true, |at| at.elapsed() >= Duration::from_millis(500))
        {
            self.checked.set(Some(Instant::now()));
            let viewable = self
                .connection
                .get_window_attributes(self.xid)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .is_some_and(|attributes| attributes.map_state == MapState::VIEWABLE);
            if !viewable {
                self.lost.store(true, Ordering::SeqCst);
            }
        }
        self.lost.load(Ordering::SeqCst)
    }
}

impl Drop for SourceWatch {
    fn drop(&mut self) {
        SOURCES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|source| !Arc::ptr_eq(&source.lost, &self.lost));
    }
}

pub(crate) fn initialize() -> Result<()> {
    static XLIB: OnceLock<std::result::Result<xlib::Xlib, String>> = OnceLock::new();
    static INITIALIZED: OnceLock<bool> = OnceLock::new();
    let xlib = XLIB
        .get_or_init(|| xlib::Xlib::open().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(backend)?;
    // Must run before GStreamer opens any Xlib display. Discovery uses x11rb,
    // which talks to the server directly and does not initialize Xlib.
    if !INITIALIZED.get_or_init(|| unsafe { (xlib.XInitThreads)() } != 0) {
        return Err(backend("XInitThreads failed"));
    }
    PREVIOUS_HANDLER.get_or_init(|| unsafe { (xlib.XSetErrorHandler)(Some(handle_error)) });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn error(resourceid: c_ulong, error_code: u8, request_code: u8) -> xlib::XErrorEvent {
        xlib::XErrorEvent {
            type_: 0,
            display: std::ptr::null_mut(),
            resourceid,
            serial: 0,
            error_code,
            request_code,
            minor_code: 0,
        }
    }

    #[test]
    fn source_errors_only_claim_the_recorded_window() {
        let source = Source {
            xid: 42,
            damage: None,
            lost: Arc::new(AtomicBool::new(false)),
        };
        assert!(!source.claims(&error(43, xlib::BadWindow, 3)));
        assert!(!source.lost.load(Ordering::SeqCst));
        assert!(source.claims(&error(42, xlib::BadWindow, 3)));
        assert!(source.lost.load(Ordering::SeqCst));
        assert!(!source.claims(&error(43, xlib::BadDrawable, 73)));
    }

    #[test]
    fn damage_cleanup_errors_require_a_lost_source_and_matching_extension() {
        let source = Source {
            xid: 42,
            damage: Some((143, 152)),
            lost: Arc::new(AtomicBool::new(false)),
        };
        assert!(!source.claims(&error(99, 152, 143)));
        assert!(source.claims(&error(42, xlib::BadDrawable, 73)));
        assert!(source.claims(&error(99, 152, 143)));
        assert!(!source.claims(&error(99, 151, 143)));
        assert!(!source.claims(&error(99, 152, 144)));
    }
}
