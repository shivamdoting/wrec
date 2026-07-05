use rusqlite::{params, Connection};
use std::{
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const SCHEMA_VERSION: i64 = 2;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("store io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("store sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, Copy)]
pub enum RecordingStatus {
    Starting,
    Recording,
    Completed,
    Failed,
}

impl RecordingStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Recording => "recording",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum EventLevel {
    Info,
    Warn,
    Error,
}

impl EventLevel {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum EventSource {
    App,
    Backend,
    CaptureEngine,
}

impl EventSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Backend => "backend",
            Self::CaptureEngine => "capture_engine",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecordingRecord {
    pub id: u64,
    pub started_at_ms: i64,
    pub output_path: PathBuf,
    pub target_kind: String,
    pub target_id: u64,
    pub target_name: String,
    pub codec: String,
    pub quality: String,
    pub resolution: String,
    pub fps: u32,
    pub include_cursor: bool,
    pub include_system_audio: bool,
}

#[derive(Debug, Clone)]
pub struct EventRecord {
    pub recording_id: Option<u64>,
    pub timestamp_ms: i64,
    pub level: EventLevel,
    pub source: EventSource,
    pub message: String,
    pub fields_json: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MetricRecord {
    pub recording_id: u64,
    pub timestamp_ms: i64,
    pub elapsed_secs: u64,
    pub output_bytes: u64,
    pub bitrate_mbps: f32,
    pub frames: Option<u64>,
    pub dropped_frames: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
pub struct CaptureDimensions {
    pub native_width: i64,
    pub native_height: i64,
    pub output_width: i64,
    pub output_height: i64,
}

#[derive(Debug)]
enum StoreCommand {
    UpsertRecording(RecordingRecord),
    MarkRecordingStarted {
        id: u64,
    },
    MarkRecordingFinished {
        id: u64,
        status: RecordingStatus,
        stopped_at_ms: i64,
        file_size_bytes: Option<u64>,
        error_message: Option<String>,
    },
    UpdateDimensions {
        id: u64,
        dimensions: CaptureDimensions,
    },
    AppendEvent(EventRecord),
    AppendMetric(MetricRecord),
    Shutdown,
}

pub struct Store {
    sender: mpsc::Sender<StoreCommand>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Store {
    pub fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(path)?;
        configure_connection(&conn)?;
        migrate(&conn)?;

        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || writer_loop(conn, receiver));

        Ok(Self {
            sender,
            handle: Some(handle),
        })
    }

    pub fn upsert_recording(&self, recording: RecordingRecord) {
        self.send(StoreCommand::UpsertRecording(recording));
    }

    pub fn mark_recording_started(&self, id: u64) {
        self.send(StoreCommand::MarkRecordingStarted { id });
    }

    pub fn mark_recording_completed(
        &self,
        id: u64,
        stopped_at_ms: i64,
        file_size_bytes: Option<u64>,
    ) {
        self.send(StoreCommand::MarkRecordingFinished {
            id,
            status: RecordingStatus::Completed,
            stopped_at_ms,
            file_size_bytes,
            error_message: None,
        });
    }

    pub fn mark_recording_failed(&self, id: u64, stopped_at_ms: i64, error_message: String) {
        self.send(StoreCommand::MarkRecordingFinished {
            id,
            status: RecordingStatus::Failed,
            stopped_at_ms,
            file_size_bytes: None,
            error_message: Some(error_message),
        });
    }

    pub fn update_dimensions(&self, id: u64, dimensions: CaptureDimensions) {
        self.send(StoreCommand::UpdateDimensions { id, dimensions });
    }

    pub fn append_event(&self, event: EventRecord) {
        self.send(StoreCommand::AppendEvent(event));
    }

    pub fn append_metric(&self, metric: MetricRecord) {
        self.send(StoreCommand::AppendMetric(metric));
    }

    fn send(&self, command: StoreCommand) {
        if self.sender.send(command).is_err() {
            tracing::warn!("store writer is not available");
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = self.sender.send(StoreCommand::Shutdown);
        if let Some(handle) = self.handle.take() {
            if handle.join().is_err() {
                tracing::warn!("store writer thread panicked");
            }
        }
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

fn configure_connection(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(Duration::from_millis(250))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let version = conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS recordings (
            id INTEGER PRIMARY KEY,
            started_at_ms INTEGER NOT NULL,
            stopped_at_ms INTEGER,
            status TEXT NOT NULL,
            output_path TEXT NOT NULL,
            target_kind TEXT NOT NULL,
            target_id INTEGER NOT NULL,
            target_name TEXT NOT NULL,
            codec TEXT NOT NULL,
            quality TEXT NOT NULL,
            resolution TEXT NOT NULL,
            fps INTEGER NOT NULL,
            include_cursor INTEGER NOT NULL,
            include_system_audio INTEGER NOT NULL DEFAULT 1,
            native_width INTEGER,
            native_height INTEGER,
            output_width INTEGER,
            output_height INTEGER,
            duration_ms INTEGER,
            file_size_bytes INTEGER,
            error_message TEXT
        );

        CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            recording_id INTEGER,
            timestamp_ms INTEGER NOT NULL,
            level TEXT NOT NULL,
            source TEXT NOT NULL,
            message TEXT NOT NULL,
            fields_json TEXT,
            FOREIGN KEY(recording_id) REFERENCES recordings(id) ON DELETE SET NULL
        );

        CREATE TABLE IF NOT EXISTS metrics (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            recording_id INTEGER NOT NULL,
            timestamp_ms INTEGER NOT NULL,
            elapsed_secs INTEGER NOT NULL,
            output_bytes INTEGER NOT NULL,
            bitrate_mbps REAL NOT NULL,
            frames INTEGER,
            dropped_frames INTEGER,
            FOREIGN KEY(recording_id) REFERENCES recordings(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_recordings_started_at ON recordings(started_at_ms);
        CREATE INDEX IF NOT EXISTS idx_recordings_status ON recordings(status);
        CREATE INDEX IF NOT EXISTS idx_events_recording_time ON events(recording_id, timestamp_ms);
        CREATE INDEX IF NOT EXISTS idx_metrics_recording_time ON metrics(recording_id, timestamp_ms);
        ",
    )?;

    if version == 1 {
        conn.execute(
            "ALTER TABLE recordings ADD COLUMN include_system_audio INTEGER NOT NULL DEFAULT 1",
            [],
        )?;
    }

    conn.pragma_update(None, "user_version", SCHEMA_VERSION)
}

impl StoreCommand {
    fn kind(&self) -> &'static str {
        match self {
            StoreCommand::UpsertRecording(_) => "upsert recording",
            StoreCommand::MarkRecordingStarted { .. } => "mark recording started",
            StoreCommand::MarkRecordingFinished { .. } => "mark recording finished",
            StoreCommand::UpdateDimensions { .. } => "update dimensions",
            StoreCommand::AppendEvent(_) => "append event",
            StoreCommand::AppendMetric(_) => "append metric",
            StoreCommand::Shutdown => "shutdown",
        }
    }
}

fn writer_loop(conn: Connection, receiver: mpsc::Receiver<StoreCommand>) {
    for command in receiver {
        let kind = command.kind();
        let result = match command {
            StoreCommand::UpsertRecording(recording) => upsert_recording(&conn, &recording),
            StoreCommand::MarkRecordingStarted { id } => mark_recording_started(&conn, id),
            StoreCommand::MarkRecordingFinished {
                id,
                status,
                stopped_at_ms,
                file_size_bytes,
                error_message,
            } => mark_recording_finished(
                &conn,
                id,
                status,
                stopped_at_ms,
                file_size_bytes,
                error_message.as_deref(),
            ),
            StoreCommand::UpdateDimensions { id, dimensions } => {
                update_dimensions(&conn, id, dimensions)
            }
            StoreCommand::AppendEvent(event) => append_event(&conn, &event),
            StoreCommand::AppendMetric(metric) => append_metric(&conn, &metric),
            StoreCommand::Shutdown => break,
        };

        if let Err(err) = result {
            // Writes are fire-and-forget, so a dropped write never reaches the
            // caller; the log line is the only trace history has a gap.
            tracing::error!("store write failed ({kind}): {err}");
        }
    }
}

fn upsert_recording(conn: &Connection, recording: &RecordingRecord) -> rusqlite::Result<()> {
    conn.execute(
        "
        INSERT INTO recordings (
            id,
            started_at_ms,
            status,
            output_path,
            target_kind,
            target_id,
            target_name,
            codec,
            quality,
            resolution,
            fps,
            include_cursor,
            include_system_audio
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        ON CONFLICT(id) DO UPDATE SET
            started_at_ms = excluded.started_at_ms,
            status = excluded.status,
            output_path = excluded.output_path,
            target_kind = excluded.target_kind,
            target_id = excluded.target_id,
            target_name = excluded.target_name,
            codec = excluded.codec,
            quality = excluded.quality,
            resolution = excluded.resolution,
            fps = excluded.fps,
            include_cursor = excluded.include_cursor,
            include_system_audio = excluded.include_system_audio,
            stopped_at_ms = NULL,
            duration_ms = NULL,
            file_size_bytes = NULL,
            error_message = NULL,
            native_width = NULL,
            native_height = NULL,
            output_width = NULL,
            output_height = NULL
        ",
        params![
            u64_to_i64(recording.id),
            recording.started_at_ms,
            RecordingStatus::Starting.as_str(),
            recording.output_path.display().to_string(),
            recording.target_kind.as_str(),
            u64_to_i64(recording.target_id),
            recording.target_name.as_str(),
            recording.codec.as_str(),
            recording.quality.as_str(),
            recording.resolution.as_str(),
            i64::from(recording.fps),
            bool_to_i64(recording.include_cursor),
            bool_to_i64(recording.include_system_audio),
        ],
    )?;
    Ok(())
}

fn mark_recording_started(conn: &Connection, id: u64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE recordings SET status = ?1, error_message = NULL WHERE id = ?2",
        params![RecordingStatus::Recording.as_str(), u64_to_i64(id)],
    )?;
    Ok(())
}

fn mark_recording_finished(
    conn: &Connection,
    id: u64,
    status: RecordingStatus,
    stopped_at_ms: i64,
    file_size_bytes: Option<u64>,
    error_message: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "
        UPDATE recordings
        SET
            status = ?1,
            stopped_at_ms = ?2,
            duration_ms = MAX(0, ?2 - started_at_ms),
            file_size_bytes = COALESCE(?3, file_size_bytes),
            error_message = ?4
        WHERE id = ?5
        ",
        params![
            status.as_str(),
            stopped_at_ms,
            file_size_bytes.map(u64_to_i64),
            error_message,
            u64_to_i64(id),
        ],
    )?;
    Ok(())
}

fn update_dimensions(
    conn: &Connection,
    id: u64,
    dimensions: CaptureDimensions,
) -> rusqlite::Result<()> {
    conn.execute(
        "
        UPDATE recordings
        SET
            native_width = ?1,
            native_height = ?2,
            output_width = ?3,
            output_height = ?4
        WHERE id = ?5
        ",
        params![
            dimensions.native_width,
            dimensions.native_height,
            dimensions.output_width,
            dimensions.output_height,
            u64_to_i64(id),
        ],
    )?;
    Ok(())
}

fn append_event(conn: &Connection, event: &EventRecord) -> rusqlite::Result<()> {
    conn.execute(
        "
        INSERT INTO events (
            recording_id,
            timestamp_ms,
            level,
            source,
            message,
            fields_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ",
        params![
            event.recording_id.map(u64_to_i64),
            event.timestamp_ms,
            event.level.as_str(),
            event.source.as_str(),
            event.message.as_str(),
            event.fields_json.as_deref(),
        ],
    )?;
    Ok(())
}

fn append_metric(conn: &Connection, metric: &MetricRecord) -> rusqlite::Result<()> {
    conn.execute(
        "
        INSERT INTO metrics (
            recording_id,
            timestamp_ms,
            elapsed_secs,
            output_bytes,
            bitrate_mbps,
            frames,
            dropped_frames
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ",
        params![
            u64_to_i64(metric.recording_id),
            metric.timestamp_ms,
            u64_to_i64(metric.elapsed_secs),
            u64_to_i64(metric.output_bytes),
            metric.bitrate_mbps,
            metric.frames.map(u64_to_i64),
            metric.dropped_frames.map(u64_to_i64),
        ],
    )?;
    Ok(())
}

fn bool_to_i64(value: bool) -> i64 {
    i64::from(value)
}

fn u64_to_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DB_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDb {
        dir: PathBuf,
    }

    impl TempDb {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "wrec-store-test-{name}-{}-{}",
                std::process::id(),
                NEXT_DB_ID.fetch_add(1, Ordering::Relaxed),
            ));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self { dir }
        }

        fn path(&self) -> PathBuf {
            self.dir.join("store.sqlite3")
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn read_connection(db: &TempDb) -> Connection {
        let conn = Connection::open(db.path()).expect("open verification connection");
        conn.pragma_update(None, "foreign_keys", "ON")
            .expect("enable foreign keys");
        conn
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |row| row.get(0))
            .expect("count query")
    }

    fn sample_recording(id: u64) -> RecordingRecord {
        RecordingRecord {
            id,
            started_at_ms: 1_000,
            output_path: PathBuf::from("/tmp/wrec/recording.mp4"),
            target_kind: "display".to_string(),
            target_id: 3,
            target_name: "Built-in Display".to_string(),
            codec: "hevc".to_string(),
            quality: "high".to_string(),
            resolution: "1080p".to_string(),
            fps: 60,
            include_cursor: true,
            include_system_audio: true,
        }
    }

    fn sample_event(recording_id: Option<u64>) -> EventRecord {
        EventRecord {
            recording_id,
            timestamp_ms: 2_000,
            level: EventLevel::Info,
            source: EventSource::Backend,
            message: "recording started".to_string(),
            fields_json: Some(r#"{"fps":60}"#.to_string()),
        }
    }

    fn sample_metric(recording_id: u64) -> MetricRecord {
        MetricRecord {
            recording_id,
            timestamp_ms: 3_000,
            elapsed_secs: 5,
            output_bytes: 1_048_576,
            bitrate_mbps: 4.5,
            frames: Some(300),
            dropped_frames: Some(2),
        }
    }

    #[test]
    fn open_creates_schema_at_current_version() {
        let db = TempDb::new("schema");
        drop(Store::open(db.path()).expect("open store"));

        let conn = read_connection(&db);
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let tables = count(
            &conn,
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' \
             AND name IN ('recordings', 'events', 'metrics')",
        );

        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(tables, 3);
    }

    #[test]
    fn open_creates_missing_parent_directories() {
        let db = TempDb::new("parents");
        let nested = db.dir.join("a").join("b").join("store.sqlite3");

        drop(Store::open(nested.clone()).expect("open store in nested path"));

        assert!(nested.exists());
    }

    #[test]
    fn reopening_existing_store_keeps_data_and_version() {
        let db = TempDb::new("reopen");
        {
            let store = Store::open(db.path()).expect("first open");
            store.upsert_recording(sample_recording(1));
        }
        drop(Store::open(db.path()).expect("second open"));

        let conn = read_connection(&db);
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();

        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM recordings"), 1);
    }

    #[test]
    fn migrate_adds_system_audio_column_to_v1_database() {
        let db = TempDb::new("migrate-v1");
        {
            let conn = Connection::open(db.path()).expect("open raw connection");
            conn.execute_batch(
                "
                CREATE TABLE recordings (
                    id INTEGER PRIMARY KEY,
                    started_at_ms INTEGER NOT NULL,
                    stopped_at_ms INTEGER,
                    status TEXT NOT NULL,
                    output_path TEXT NOT NULL,
                    target_kind TEXT NOT NULL,
                    target_id INTEGER NOT NULL,
                    target_name TEXT NOT NULL,
                    codec TEXT NOT NULL,
                    quality TEXT NOT NULL,
                    resolution TEXT NOT NULL,
                    fps INTEGER NOT NULL,
                    include_cursor INTEGER NOT NULL,
                    native_width INTEGER,
                    native_height INTEGER,
                    output_width INTEGER,
                    output_height INTEGER,
                    duration_ms INTEGER,
                    file_size_bytes INTEGER,
                    error_message TEXT
                );
                INSERT INTO recordings (
                    id, started_at_ms, status, output_path, target_kind, target_id,
                    target_name, codec, quality, resolution, fps, include_cursor
                ) VALUES (
                    7, 1000, 'completed', '/tmp/old.mp4', 'display', 1,
                    'Main', 'hevc', 'high', 'native', 60, 1
                );
                PRAGMA user_version = 1;
                ",
            )
            .expect("create v1 schema");
        }

        drop(Store::open(db.path()).expect("open migrates v1 store"));

        let conn = read_connection(&db);
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let audio: i64 = conn
            .query_row(
                "SELECT include_system_audio FROM recordings WHERE id = 7",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(audio, 1);
    }

    #[test]
    fn upsert_recording_inserts_row_with_starting_status() {
        let db = TempDb::new("insert");
        {
            let store = Store::open(db.path()).expect("open store");
            store.upsert_recording(sample_recording(1));
        }

        let conn = read_connection(&db);
        let row = conn
            .query_row(
                "SELECT status, output_path, target_name, fps, include_cursor, \
                 include_system_audio FROM recordings WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(row.0, "starting");
        assert_eq!(row.1, "/tmp/wrec/recording.mp4");
        assert_eq!(row.2, "Built-in Display");
        assert_eq!(row.3, 60);
        assert_eq!(row.4, 1);
        assert_eq!(row.5, 1);
    }

    #[test]
    fn upsert_recording_replaces_existing_row_with_same_id() {
        let db = TempDb::new("upsert");
        {
            let store = Store::open(db.path()).expect("open store");
            store.upsert_recording(sample_recording(1));
            store.mark_recording_completed(1, 5_000, Some(100));

            let mut replacement = sample_recording(1);
            replacement.codec = "h264".to_string();
            replacement.fps = 30;
            store.upsert_recording(replacement);
        }

        let conn = read_connection(&db);
        let row = conn
            .query_row(
                "SELECT status, codec, fps, stopped_at_ms, duration_ms, file_size_bytes
                 FROM recordings WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(count(&conn, "SELECT COUNT(*) FROM recordings"), 1);
        assert_eq!(row.0, "starting");
        assert_eq!(row.1, "h264");
        assert_eq!(row.2, 30);
        assert_eq!(row.3, None);
        assert_eq!(row.4, None);
        assert_eq!(row.5, None);
    }

    #[test]
    fn mark_recording_started_sets_status_and_clears_error() {
        let db = TempDb::new("started");
        {
            let store = Store::open(db.path()).expect("open store");
            store.upsert_recording(sample_recording(1));
            store.mark_recording_failed(1, 2_000, "engine crashed".to_string());
            store.mark_recording_started(1);
        }

        let conn = read_connection(&db);
        let (status, error): (String, Option<String>) = conn
            .query_row(
                "SELECT status, error_message FROM recordings WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(status, "recording");
        assert_eq!(error, None);
    }

    #[test]
    fn mark_recording_completed_sets_duration_and_file_size() {
        let db = TempDb::new("completed");
        {
            let store = Store::open(db.path()).expect("open store");
            store.upsert_recording(sample_recording(1));
            store.mark_recording_completed(1, 5_000, Some(2_048));
        }

        let conn = read_connection(&db);
        let row = conn
            .query_row(
                "SELECT status, stopped_at_ms, duration_ms, file_size_bytes, error_message \
                 FROM recordings WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(row.0, "completed");
        assert_eq!(row.1, 5_000);
        assert_eq!(row.2, 4_000);
        assert_eq!(row.3, 2_048);
        assert_eq!(row.4, None);
    }

    #[test]
    fn completing_without_file_size_keeps_previous_value() {
        let db = TempDb::new("keep-size");
        {
            let store = Store::open(db.path()).expect("open store");
            store.upsert_recording(sample_recording(1));
            store.mark_recording_completed(1, 5_000, Some(2_048));
            store.mark_recording_completed(1, 6_000, None);
        }

        let conn = read_connection(&db);
        let size: i64 = conn
            .query_row(
                "SELECT file_size_bytes FROM recordings WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(size, 2_048);
    }

    #[test]
    fn duration_clamps_to_zero_when_stop_precedes_start() {
        let db = TempDb::new("clamp-duration");
        {
            let store = Store::open(db.path()).expect("open store");
            store.upsert_recording(sample_recording(1));
            store.mark_recording_completed(1, 500, None);
        }

        let conn = read_connection(&db);
        let duration: i64 = conn
            .query_row(
                "SELECT duration_ms FROM recordings WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(duration, 0);
    }

    #[test]
    fn mark_recording_failed_records_error_message() {
        let db = TempDb::new("failed");
        {
            let store = Store::open(db.path()).expect("open store");
            store.upsert_recording(sample_recording(1));
            store.mark_recording_failed(1, 4_000, "engine crashed".to_string());
        }

        let conn = read_connection(&db);
        let row = conn
            .query_row(
                "SELECT status, error_message, file_size_bytes FROM recordings WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(row.0, "failed");
        assert_eq!(row.1.as_deref(), Some("engine crashed"));
        assert_eq!(row.2, None);
    }

    #[test]
    fn updates_for_unknown_recording_change_nothing() {
        let db = TempDb::new("missing-row");
        {
            let store = Store::open(db.path()).expect("open store");
            store.mark_recording_started(99);
            store.mark_recording_completed(99, 5_000, Some(1));
            store.update_dimensions(
                99,
                CaptureDimensions {
                    native_width: 1,
                    native_height: 1,
                    output_width: 1,
                    output_height: 1,
                },
            );
        }

        let conn = read_connection(&db);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM recordings"), 0);
    }

    #[test]
    fn update_dimensions_sets_capture_columns() {
        let db = TempDb::new("dimensions");
        {
            let store = Store::open(db.path()).expect("open store");
            store.upsert_recording(sample_recording(1));
            store.update_dimensions(
                1,
                CaptureDimensions {
                    native_width: 3024,
                    native_height: 1964,
                    output_width: 1512,
                    output_height: 982,
                },
            );
        }

        let conn = read_connection(&db);
        let row = conn
            .query_row(
                "SELECT native_width, native_height, output_width, output_height \
                 FROM recordings WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(row, (3024, 1964, 1512, 982));
    }

    #[test]
    fn append_event_persists_all_fields() {
        let db = TempDb::new("event");
        {
            let store = Store::open(db.path()).expect("open store");
            store.upsert_recording(sample_recording(1));
            store.append_event(sample_event(Some(1)));
        }

        let conn = read_connection(&db);
        let row = conn
            .query_row(
                "SELECT recording_id, timestamp_ms, level, source, message, fields_json \
                 FROM events",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(row.0, 1);
        assert_eq!(row.1, 2_000);
        assert_eq!(row.2, "info");
        assert_eq!(row.3, "backend");
        assert_eq!(row.4, "recording started");
        assert_eq!(row.5.as_deref(), Some(r#"{"fps":60}"#));
    }

    #[test]
    fn append_event_without_recording_id_is_allowed() {
        let db = TempDb::new("event-detached");
        {
            let store = Store::open(db.path()).expect("open store");
            store.append_event(EventRecord {
                recording_id: None,
                fields_json: None,
                ..sample_event(None)
            });
        }

        let conn = read_connection(&db);
        let (recording_id, fields_json): (Option<i64>, Option<String>) = conn
            .query_row("SELECT recording_id, fields_json FROM events", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();

        assert_eq!(recording_id, None);
        assert_eq!(fields_json, None);
    }

    #[test]
    fn event_with_unknown_recording_id_is_dropped() {
        let db = TempDb::new("event-orphan");
        {
            let store = Store::open(db.path()).expect("open store");
            store.append_event(sample_event(Some(42)));
            store.append_event(sample_event(None));
        }

        let conn = read_connection(&db);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM events"), 1);
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM events WHERE recording_id IS NULL"
            ),
            1
        );
    }

    #[test]
    fn append_metric_persists_all_fields() {
        let db = TempDb::new("metric");
        {
            let store = Store::open(db.path()).expect("open store");
            store.upsert_recording(sample_recording(1));
            store.append_metric(sample_metric(1));
            store.append_metric(MetricRecord {
                timestamp_ms: 4_000,
                frames: None,
                dropped_frames: None,
                ..sample_metric(1)
            });
        }

        let conn = read_connection(&db);
        let row = conn
            .query_row(
                "SELECT recording_id, timestamp_ms, elapsed_secs, output_bytes, bitrate_mbps, \
                 frames, dropped_frames FROM metrics ORDER BY timestamp_ms LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, f64>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                    ))
                },
            )
            .unwrap();
        let (frames, dropped): (Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT frames, dropped_frames FROM metrics WHERE timestamp_ms = 4000",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(row.0, 1);
        assert_eq!(row.1, 3_000);
        assert_eq!(row.2, 5);
        assert_eq!(row.3, 1_048_576);
        assert!((row.4 - 4.5).abs() < 1e-6);
        assert_eq!(row.5, Some(300));
        assert_eq!(row.6, Some(2));
        assert_eq!(frames, None);
        assert_eq!(dropped, None);
    }

    #[test]
    fn deleting_recording_cascades_metrics_and_detaches_events() {
        let db = TempDb::new("cascade");
        {
            let store = Store::open(db.path()).expect("open store");
            store.upsert_recording(sample_recording(1));
            store.append_event(sample_event(Some(1)));
            store.append_metric(sample_metric(1));
        }

        let conn = read_connection(&db);
        conn.execute("DELETE FROM recordings WHERE id = 1", [])
            .expect("delete recording");

        assert_eq!(count(&conn, "SELECT COUNT(*) FROM metrics"), 0);
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM events WHERE recording_id IS NULL"
            ),
            1
        );
    }

    #[test]
    fn u64_values_above_i64_max_are_clamped() {
        assert_eq!(u64_to_i64(u64::MAX), i64::MAX);
        assert_eq!(u64_to_i64(42), 42);

        let db = TempDb::new("clamp-id");
        {
            let store = Store::open(db.path()).expect("open store");
            store.upsert_recording(sample_recording(u64::MAX));
        }

        let conn = read_connection(&db);
        let id: i64 = conn
            .query_row("SELECT id FROM recordings", [], |row| row.get(0))
            .unwrap();

        assert_eq!(id, i64::MAX);
    }

    #[test]
    fn now_ms_returns_positive_timestamp() {
        assert!(now_ms() > 0);
    }
}
