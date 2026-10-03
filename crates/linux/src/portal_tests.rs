use super::*;
use std::{
    collections::HashMap,
    io::{Read, Write},
    os::{fd::AsRawFd, unix::net::UnixStream},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};
use zbus::{
    message::Header,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
    Connection,
};

#[derive(Clone, Default)]
struct MockPortal {
    mode: Arc<AtomicU32>,
    closes: Arc<AtomicU32>,
    remotes: Arc<AtomicU32>,
    peers: Arc<std::sync::Mutex<Vec<UnixStream>>>,
    sessions: Arc<std::sync::Mutex<Vec<OwnedObjectPath>>>,
    selections: Arc<std::sync::Mutex<Vec<(u32, u32, bool)>>>,
}

struct MockSession(Arc<AtomicU32>);

#[zbus::interface(name = "org.freedesktop.portal.Session")]
impl MockSession {
    fn close(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

fn path(
    header: &Header<'_>,
    options: &HashMap<String, OwnedValue>,
    session: bool,
) -> OwnedObjectPath {
    let sender = header
        .sender()
        .unwrap()
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_");
    let (kind, key) = if session {
        ("session", "session_handle_token")
    } else {
        ("request", "handle_token")
    };
    let token = <&str>::try_from(options.get(key).unwrap()).unwrap();
    format!("/org/freedesktop/portal/desktop/{kind}/{sender}/{token}")
        .try_into()
        .unwrap()
}

async fn response(
    connection: &Connection,
    request: &OwnedObjectPath,
    code: u32,
    data: HashMap<&str, Value<'_>>,
) {
    connection
        .emit_signal(
            None::<&str>,
            request,
            "org.freedesktop.portal.Request",
            "Response",
            &(code, data),
        )
        .await
        .unwrap();
}

#[zbus::interface(name = "org.freedesktop.portal.ScreenCast")]
impl MockPortal {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        5
    }
    #[zbus(property)]
    fn available_source_types(&self) -> u32 {
        3
    }
    #[zbus(property)]
    fn available_cursor_modes(&self) -> u32 {
        3
    }

    async fn create_session(
        &self,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> OwnedObjectPath {
        let session = path(&header, &options, true);
        let request = path(&header, &options, false);
        self.sessions.lock().unwrap().push(session.clone());
        connection
            .object_server()
            .at(session.clone(), MockSession(self.closes.clone()))
            .await
            .unwrap();
        response(
            connection,
            &request,
            0,
            HashMap::from([("session_handle", Value::from(session.as_str()))]),
        )
        .await;
        request
    }

    async fn select_sources(
        &self,
        _session: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> OwnedObjectPath {
        self.selections.lock().unwrap().push((
            u32::try_from(&options["types"]).unwrap(),
            u32::try_from(&options["cursor_mode"]).unwrap(),
            bool::try_from(&options["multiple"]).unwrap(),
        ));
        let request = path(&header, &options, false);
        response(connection, &request, 0, HashMap::new()).await;
        request
    }

    async fn start(
        &self,
        _session: OwnedObjectPath,
        _parent: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> OwnedObjectPath {
        let request = path(&header, &options, false);
        match self.mode.load(Ordering::Relaxed) {
            0 => {
                let streams = vec![(42u32, HashMap::<&str, Value<'_>>::new())];
                response(
                    connection,
                    &request,
                    0,
                    HashMap::from([("streams", Value::from(streams))]),
                )
                .await;
            }
            1 => response(connection, &request, 1, HashMap::new()).await,
            _ => {} // Leave the picker pending until wrec cancels it.
        }
        request
    }

    fn open_pipe_wire_remote(
        &self,
        _session: OwnedObjectPath,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::zvariant::OwnedFd {
        self.remotes.fetch_add(1, Ordering::Relaxed);
        let (socket, peer) = UnixStream::pair().unwrap();
        self.peers.lock().unwrap().push(peer);
        let fd: OwnedFd = socket.into();
        fd.into()
    }
}

#[test]
#[ignore = "requires an isolated D-Bus: dbus-run-session -- cargo test -p linux portal_roundtrip -- --ignored"]
fn portal_roundtrip_and_cancellation_close_sessions() {
    std::env::set_var("WAYLAND_DISPLAY", "wayland-test");
    std::env::set_var("XDG_RUNTIME_DIR", std::env::temp_dir());
    let mock = MockPortal::default();
    runtime().unwrap().block_on(async {
        let server = zbus::connection::Builder::session()
            .unwrap()
            .name("org.freedesktop.portal.Desktop")
            .unwrap()
            .serve_at("/org/freedesktop/portal/desktop", mock.clone())
            .unwrap()
            .build()
            .await
            .unwrap();
        tokio::task::spawn_blocking(|| {
            assert!(!list_targets().unwrap().is_empty());
            assert!(!list_targets().unwrap().is_empty());
        })
        .await
        .unwrap();
        let target = CaptureTarget {
            id: 0,
            name: "picker".into(),
            kind: CaptureSourceKind::Display,
        };
        let (_stop, mut stopped) = watch::channel(false);
        let capture =
            tokio::time::timeout(Duration::from_secs(5), open(&target, true, &mut stopped))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(capture.connect().await.unwrap().node, 42);
        assert_eq!(capture.connect().await.unwrap().node, 42);
        assert_eq!(mock.remotes.load(Ordering::Relaxed), 2);
        // Retrying the encoder opens a new remote, not a second source picker.
        assert_eq!(mock.selections.lock().unwrap().len(), 1);
        capture.close().await;
        assert_eq!(mock.closes.load(Ordering::Relaxed), 1);
        assert_eq!(*mock.selections.lock().unwrap(), vec![(1, 2, false)]);

        mock.mode.store(1, Ordering::Relaxed);
        let denied =
            tokio::time::timeout(Duration::from_secs(5), open(&target, false, &mut stopped))
                .await
                .unwrap();
        assert!(denied.is_err());
        assert_eq!(mock.closes.load(Ordering::Relaxed), 2);

        mock.mode.store(2, Ordering::Relaxed);
        let (stop, mut stopped) = watch::channel(false);
        let cancelled = open(&target, true, &mut stopped);
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            stop.send(true).unwrap();
        };
        let (result, ()) = tokio::join!(cancelled, cancel);
        assert!(matches!(result, Err(RecorderError::Cancelled)));
        assert_eq!(mock.closes.load(Ordering::Relaxed), 3);

        mock.mode.store(0, Ordering::Relaxed);
        let (_stop, mut stopped) = watch::channel(false);
        let capture = open(&target, true, &mut stopped).await.unwrap();
        let mut endings = capture.endings().await.unwrap();
        let stream = capture.connect().await.unwrap();
        assert_eq!(stream.lost(), None);
        let peer = mock.peers.lock().unwrap().pop().unwrap();
        (&peer).write_all(b"pipewire").unwrap();
        drop(peer);
        assert_eq!(stream.lost(), Some(PIPEWIRE_LOST));
        let mut unread = Vec::new();
        UnixStream::from(stream.fd.try_clone().unwrap())
            .read_to_end(&mut unread)
            .unwrap();
        assert_eq!(
            unread, b"pipewire",
            "loss detection must not consume PipeWire data"
        );

        let session = mock.sessions.lock().unwrap().last().unwrap().clone();
        server
            .emit_signal(
                None::<&str>,
                &session,
                "org.freedesktop.portal.Session",
                "Closed",
                &(HashMap::<&str, Value<'_>>::new(),),
            )
            .await
            .unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), endings.next())
                .await
                .unwrap(),
            Ending::Closed
        ));
        capture.close().await;

        let capture = open(&target, true, &mut stopped).await.unwrap();
        let mut endings = capture.endings().await.unwrap();
        let stream = capture.connect().await.unwrap();
        assert_eq!(stream.lost(), None);
        server.release_name(PORTAL).await.unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), endings.next())
                .await
                .unwrap(),
            Ending::PortalLost
        ));
        assert_eq!(stream.lost(), Some(PORTAL_LOST));
        assert!(matches!(endings.next().await, Ending::PortalLost));
        drop(endings);
        capture.close().await;

        server.request_name(PORTAL).await.unwrap();
        let capture = open(&target, true, &mut stopped).await.unwrap();
        let stream = capture.connect().await.unwrap();
        assert_eq!(stream.lost(), None);
        assert!(capture.endings().await.is_ok());
        capture.close().await;
    });
}

struct PrivatePipeWire {
    daemon: std::process::Child,
    directory: std::path::PathBuf,
}

impl PrivatePipeWire {
    fn start() -> Self {
        let directory = std::env::temp_dir().join(format!("wrec-pipewire-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let daemon = std::process::Command::new("pipewire")
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .env("PIPEWIRE_RUNTIME_DIR", &directory)
            .env("XDG_RUNTIME_DIR", &directory)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("install pipewire to run this test");
        let pipewire = Self { daemon, directory };
        for _ in 0..50 {
            if pipewire.directory.join("pipewire-0").exists() {
                return pipewire;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("private PipeWire did not create its socket")
    }

    fn connect(&self) -> OwnedFd {
        UnixStream::connect(self.directory.join("pipewire-0"))
            .unwrap()
            .into()
    }

    fn stream(&self, node: u32, lost: &Arc<OnceLock<&'static str>>) -> PipeWireStream {
        PipeWireStream {
            fd: self.connect(),
            node,
            lost: lost.clone(),
        }
    }

    fn node(&self, name: &str) -> u32 {
        let provider = gst::DeviceProviderFactory::by_name("pipewiredeviceprovider").unwrap();
        for _ in 0..50 {
            let fd = self.connect();
            provider.set_property("fd", fd.as_raw_fd());
            provider.start().unwrap();
            let found = provider.devices().iter().find_map(|device| {
                let properties = device.properties()?;
                (properties.get::<String>("node.name").ok()? == name)
                    .then(|| node_id(device))
                    .flatten()
            });
            provider.stop();
            if let Some(node) = found {
                return node;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("PipeWire node {name} did not appear")
    }
}

impl Drop for PrivatePipeWire {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn wait_for(lost: &Arc<OnceLock<&'static str>>) -> Option<&'static str> {
    for _ in 0..50 {
        if let Some(message) = lost.get() {
            return Some(message);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

#[test]
#[ignore = "requires the pipewire daemon and GStreamer PipeWire plugin: cargo test -p linux node_watch -- --ignored"]
fn node_watch_reports_removal_absence_and_stops_after_daemon_loss() {
    gst::init().unwrap();
    let mut pipewire = PrivatePipeWire::start();
    let source = gst::parse::launch(
        "videotestsrc is-live=true ! pipewiresink name=sink client-name=wrec-node-watch",
    )
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    let sink_fd = pipewire.connect();
    source
        .by_name("sink")
        .unwrap()
        .set_property("fd", sink_fd.as_raw_fd());
    source.set_state(gst::State::Playing).unwrap();
    let node = pipewire.node("wrec-node-watch");

    let lost = Arc::new(OnceLock::new());
    let watch = runtime()
        .unwrap()
        .block_on(NodeWatch::start(pipewire.stream(node, &lost)));
    assert!(matches!(watch, NodeWatch::Watching { .. }));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        lost.get(),
        None,
        "a live, idle node must not be reported lost"
    );
    source.set_state(gst::State::Null).unwrap();
    assert_eq!(wait_for(&lost), Some(NODE_LOST));
    let at = std::time::Instant::now();
    drop(watch);
    assert!(at.elapsed() < Duration::from_secs(1), "{:?}", at.elapsed());

    let missing = Arc::new(OnceLock::new());
    let watch = runtime()
        .unwrap()
        .block_on(NodeWatch::start(pipewire.stream(node, &missing)));
    assert!(matches!(watch, NodeWatch::Unavailable(_)));
    assert_eq!(missing.get(), None);
    drop(watch);

    let restarted_fd = pipewire.connect();
    source
        .by_name("sink")
        .unwrap()
        .set_property("fd", restarted_fd.as_raw_fd());
    source.set_state(gst::State::Playing).unwrap();
    let node = pipewire.node("wrec-node-watch");
    let remote = Arc::new(OnceLock::new());
    let watch = runtime()
        .unwrap()
        .block_on(NodeWatch::start(pipewire.stream(node, &remote)));
    assert!(matches!(watch, NodeWatch::Watching { .. }));
    let stream = pipewire.stream(node, &remote);
    assert_eq!(stream.lost(), None);
    let _ = pipewire.daemon.kill();
    let _ = pipewire.daemon.wait();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(stream.lost(), Some(PIPEWIRE_LOST));
    let at = std::time::Instant::now();
    drop(watch);
    assert!(at.elapsed() < Duration::from_secs(4), "{:?}", at.elapsed());
    let _ = source.set_state(gst::State::Null);
}
