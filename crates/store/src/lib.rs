use rusqlite::{params, Connection};
use std::{
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const SCHEMA_VERSION: i64 = 1;

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
    Helper,
}

impl EventSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Backend => "backend",
            Self::Helper => "helper",
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

        PRAGMA user_version = 1;
        ",
    )
}

fn writer_loop(conn: Connection, receiver: mpsc::Receiver<StoreCommand>) {
    for command in receiver {
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
            tracing::warn!("store write failed: {err}");
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
            include_cursor
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
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
            include_cursor = excluded.include_cursor
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
