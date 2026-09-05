use crate::{backend, portal, x11};
use domain::{CaptureTarget, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Desktop {
    Wayland,
    X11,
}

pub(crate) fn detect() -> Result<Desktop> {
    detect_values(
        std::env::var_os("WAYLAND_DISPLAY").is_some_and(|s| !s.is_empty()),
        std::env::var_os("DISPLAY").is_some_and(|s| !s.is_empty()),
    )
}

fn detect_values(wayland: bool, x11: bool) -> Result<Desktop> {
    // DISPLAY in a Wayland session is usually XWayland, which cannot capture
    // the full Wayland desktop. Use the portal whenever Wayland is active.
    match (wayland, x11) {
        (true, _) => Ok(Desktop::Wayland),
        (false, true) => Ok(Desktop::X11),
        _ => Err(backend("Linux recording requires a Wayland or X11 desktop session. This process has neither WAYLAND_DISPLAY nor DISPLAY.")),
    }
}

pub fn check_desktop() -> Result<()> {
    detect().map(|_| ())
}

pub fn list_targets() -> Result<Vec<CaptureTarget>> {
    match detect()? {
        Desktop::Wayland => portal::list_targets(),
        Desktop::X11 => x11::list_targets(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chooses_portal_over_xwayland_and_supports_x11_only() {
        assert_eq!(detect_values(true, true).unwrap(), Desktop::Wayland);
        assert_eq!(detect_values(false, true).unwrap(), Desktop::X11);
        assert!(detect_values(false, false).is_err());
    }
}
