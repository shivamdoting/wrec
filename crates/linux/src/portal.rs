use crate::backend;
use ashpd::{
    desktop::{
        screencast::{CursorMode, Screencast, SourceType},
        PersistMode, Session,
    },
    WindowIdentifier,
};
use domain::{CaptureSourceKind, CaptureTarget, RecorderError, Result};
use std::{os::fd::OwnedFd, time::Duration};
use tokio::sync::watch;

#[cfg(test)]
#[path = "portal_tests.rs"]
mod tests;

pub fn check_desktop() -> Result<()> {
    if std::env::var_os("WAYLAND_DISPLAY").is_none()
        || std::env::var_os("XDG_RUNTIME_DIR").is_none()
    {
        return Err(backend("Wayland capture requires WAYLAND_DISPLAY and XDG_RUNTIME_DIR from the logged-in desktop session."));
    }
    Ok(())
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(backend)
}

pub fn list_targets() -> Result<Vec<CaptureTarget>> {
    check_desktop()?;
    runtime()?.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            let portal = Screencast::new().await.map_err(backend)?;
            let types = portal.available_source_types().await.map_err(backend)?;
            let mut targets = Vec::new();
            for (source, kind, name) in [
                (
                    SourceType::Monitor,
                    CaptureSourceKind::Display,
                    "Choose a display in the desktop picker",
                ),
                (
                    SourceType::Window,
                    CaptureSourceKind::Window,
                    "Choose a window in the desktop picker",
                ),
            ] {
                if types.contains(source) {
                    targets.push(CaptureTarget {
                        id: 0,
                        kind,
                        name: name.into(),
                    });
                }
            }
            Ok(targets)
        })
        .await
        .map_err(|_| backend("ScreenCast portal did not respond within 5s"))?
    })
}

pub(crate) fn validate_target(target: &CaptureTarget) -> Result<()> {
    if target.id != 0 {
        return Err(backend("Wayland targets use display:0 or window:0 to open the desktop picker; application names and native window IDs are unavailable."));
    }
    Ok(())
}

pub(crate) struct Capture {
    node: u32,
    pub session: Session<'static, Screencast<'static>>,
    // Keep the portal connection alive until the pipeline and session close.
    portal: Screencast<'static>,
}

pub(crate) struct PipeWireStream {
    pub fd: OwnedFd,
    pub node: u32,
}

impl Capture {
    pub async fn connect(&self) -> Result<PipeWireStream> {
        let fd = tokio::time::timeout(
            Duration::from_secs(5),
            self.portal.open_pipe_wire_remote(&self.session),
        )
        .await
        .map_err(|_| backend("Opening the PipeWire remote timed out"))?
        .map_err(backend)?;
        Ok(PipeWireStream {
            fd,
            node: self.node,
        })
    }

    pub async fn close(&self) {
        let _ = tokio::time::timeout(Duration::from_secs(3), self.session.close()).await;
    }
}

pub(crate) async fn open(
    target: &CaptureTarget,
    cursor: bool,
    stop: &mut watch::Receiver<bool>,
) -> Result<Capture> {
    if *stop.borrow() {
        return Err(RecorderError::Cancelled);
    }
    let portal = tokio::time::timeout(Duration::from_secs(5), Screencast::new())
        .await
        .map_err(|_| backend("ScreenCast portal connection timed out"))?
        .map_err(backend)?;
    let session = tokio::time::timeout(Duration::from_secs(5), portal.create_session())
        .await
        .map_err(|_| backend("ScreenCast session creation timed out"))?
        .map_err(backend)?;
    let selected = async {
        let source = match target.kind {
            CaptureSourceKind::Display => SourceType::Monitor,
            CaptureSourceKind::Window => SourceType::Window,
        };
        let cursor = if cursor {
            CursorMode::Embedded
        } else {
            CursorMode::Hidden
        };
        if !portal
            .available_cursor_modes()
            .await
            .map_err(backend)?
            .contains(cursor)
        {
            return Err(backend(
                "The desktop portal cannot provide the requested cursor mode.",
            ));
        }
        portal
            .select_sources(
                &session,
                cursor,
                source.into(),
                false,
                None,
                PersistMode::DoNot,
            )
            .await
            .map_err(backend)?
            .response()
            .map_err(backend)?;
        let response = portal
            .start(&session, &WindowIdentifier::default())
            .await
            .map_err(backend)?
            .response()
            .map_err(backend)?;
        if response.streams().len() != 1 {
            return Err(backend(
                "The desktop portal must return exactly one capture stream.",
            ));
        }
        let node = response.streams()[0].pipe_wire_node_id();
        Ok(node)
    };
    let result = tokio::select! {
        result = selected => result,
        _ = stop.changed() => Err(RecorderError::Cancelled),
        _ = tokio::time::sleep(Duration::from_secs(120)) => Err(backend("Desktop source selection timed out after 120s; start again and choose a source.")),
    };
    match result {
        Ok(node) => Ok(Capture {
            node,
            session,
            portal,
        }),
        Err(error) => {
            let _ = tokio::time::timeout(Duration::from_secs(3), session.close()).await;
            Err(error)
        }
    }
}
