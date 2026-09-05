use super::*;
use std::{
    collections::HashMap,
    os::unix::net::UnixStream,
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
        let (socket, _peer) = UnixStream::pair().unwrap();
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
        let _server = zbus::connection::Builder::session()
            .unwrap()
            .name("org.freedesktop.portal.Desktop")
            .unwrap()
            .serve_at("/org/freedesktop/portal/desktop", mock.clone())
            .unwrap()
            .build()
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
        assert_eq!(capture.stream.node, 42);
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
    });
}
