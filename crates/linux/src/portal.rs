use crate::backend;
use ashpd::{
    desktop::{
        screencast::{CursorMode, Screencast, SourceType},
        PersistMode, Session,
    },
    WindowIdentifier,
};
use domain::{CaptureSourceKind, CaptureTarget, RecorderError, Result};
use futures_util::{Stream, StreamExt};
use gstreamer::{self as gst, prelude::*};
use std::{
    os::fd::{AsRawFd, OwnedFd},
    pin::Pin,
    sync::{mpsc, Arc, OnceLock},
    thread,
    time::Duration,
};
use tokio::sync::{oneshot, watch};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
pub(crate) const PIPEWIRE_LOST: &str = "The PipeWire connection closed, so the screen recording stopped. Restart PipeWire and WirePlumber (or the desktop session), then record again.";
pub(crate) const NODE_LOST: &str = "The desktop's screen-cast stream ended because its PipeWire node was removed, so the screen recording stopped. If the portal backend (such as xdg-desktop-portal-wlr) or the compositor exited, restart it, then record again.";
pub(crate) const PORTAL_LOST: &str = "The desktop portal (org.freedesktop.portal.Desktop) exited, so the screen recording stopped. Restart xdg-desktop-portal and its desktop backend, then record again.";

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

pub(crate) fn runtime() -> Result<&'static tokio::runtime::Runtime> {
    // ashpd caches its D-Bus connection globally. Its I/O tasks must outlive
    // individual target queries and recordings, even when called on new threads.
    static RUNTIME: std::sync::OnceLock<std::result::Result<tokio::runtime::Runtime, String>> =
        std::sync::OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
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
    lost: Arc<OnceLock<&'static str>>,
}

pub(crate) struct PipeWireStream {
    pub fd: OwnedFd,
    pub node: u32,
    lost: Arc<OnceLock<&'static str>>,
}

impl PipeWireStream {
    fn lose(&self, message: &'static str) {
        let _ = self.lost.set(message);
    }

    pub(crate) fn lost(&self) -> Option<&'static str> {
        if let Some(message) = self.lost.get() {
            return Some(message);
        }
        let mut remote = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLRDHUP,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut remote, 1, 0) };
        let closed = libc::POLLHUP | libc::POLLRDHUP | libc::POLLERR | libc::POLLNVAL;
        (ready > 0 && remote.revents & closed != 0).then_some(PIPEWIRE_LOST)
    }
}

pub(crate) enum NodeWatch {
    Watching {
        stop: mpsc::Sender<()>,
        done: mpsc::Receiver<()>,
    },
    Unavailable(String),
}

impl NodeWatch {
    pub(crate) async fn start(remote: PipeWireStream) -> Self {
        match Self::watch(remote).await {
            Ok(watch) => watch,
            Err(error) => Self::Unavailable(error.to_string()),
        }
    }

    async fn watch(remote: PipeWireStream) -> Result<Self> {
        gst::init().map_err(backend)?;
        let provider = gst::DeviceProviderFactory::by_name("pipewiredeviceprovider")
            .ok_or_else(|| backend("GStreamer's PipeWire device provider is not installed"))?;
        provider.set_property("fd", remote.fd.as_raw_fd());
        let bus = provider.bus();
        let (stop, stops) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let (ready, synced) = oneshot::channel();
        thread::Builder::new()
            .name("wrec-pipewire-node".into())
            .spawn(move || {
                let present = provider.start().is_ok()
                    && provider
                        .devices()
                        .iter()
                        .any(|device| node_id(device) == Some(remote.node));
                let _ = ready.send(present);
                while present && matches!(stops.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                    let removed = bus
                        .timed_pop_filtered(
                            gst::ClockTime::from_mseconds(100),
                            &[gst::MessageType::DeviceRemoved],
                        )
                        .is_some_and(|message| match message.view() {
                            gst::MessageView::DeviceRemoved(removed) => {
                                node_id(&removed.device()) == Some(remote.node)
                            }
                            _ => false,
                        });
                    if removed {
                        remote.lose(NODE_LOST);
                        break;
                    }
                }
                provider.stop();
                drop(remote);
                let _ = finished.send(());
            })
            .map_err(backend)?;
        let watch = Self::Watching { stop, done };
        match tokio::time::timeout(Duration::from_secs(5), synced).await {
            Ok(Ok(true)) => Ok(watch),
            Ok(_) => Err(backend(
                "the selected node was not visible on a second PipeWire connection",
            )),
            Err(_) => Err(backend("PipeWire did not list its nodes within 5s")),
        }
    }
}

impl Drop for NodeWatch {
    fn drop(&mut self) {
        if let Self::Watching { stop, done } = self {
            let _ = stop.send(());
            let _ = done.recv_timeout(Duration::from_secs(3));
        }
    }
}

fn node_id(device: &gst::Device) -> Option<u32> {
    device
        .has_property("id", Some(gst::glib::Type::U32))
        .then(|| device.property::<u32>("id"))
}

pub(crate) enum Ending {
    Closed,
    PortalLost,
}

pub(crate) struct Endings<'a> {
    closed: Pin<Box<dyn Stream<Item = ()> + 'a>>,
    owners: ashpd::zbus::proxy::OwnerChangedStream<'a>,
    lost: Arc<OnceLock<&'static str>>,
}

impl Endings<'_> {
    pub(crate) async fn next(&mut self) -> Ending {
        if self.lost.get().is_some() {
            return Ending::PortalLost;
        }
        tokio::select! {
            _ = self.closed.next() => Ending::Closed,
            _ = self.owners.next() => {
                let _ = self.lost.set(PORTAL_LOST);
                Ending::PortalLost
            }
        }
    }
}

impl Capture {
    pub async fn endings(&self) -> Result<Endings<'_>> {
        let closed = self.session.receive_closed().await.map_err(backend)?;
        let owners = self.portal.receive_owner_changed().await.map_err(backend)?;
        let present = ashpd::zbus::fdo::DBusProxy::new(self.portal.connection())
            .await
            .map_err(backend)?
            .name_has_owner(PORTAL.try_into().map_err(backend)?)
            .await
            .map_err(backend)?;
        if !present {
            let _ = self.lost.set(PORTAL_LOST);
        }
        Ok(Endings {
            closed: Box::pin(closed.map(|_| ())),
            owners,
            lost: self.lost.clone(),
        })
    }

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
            lost: self.lost.clone(),
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
            lost: Arc::new(OnceLock::new()),
        }),
        Err(error) => {
            let _ = tokio::time::timeout(Duration::from_secs(3), session.close()).await;
            Err(error)
        }
    }
}
