//! Opt-in local benchmark. Observations are not ground-truth labels.
use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::{BufWriter, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    sync::Mutex,
    time::Duration,
};

use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(default)]
pub struct BenchmarkConfig {
    pub directory: Option<String>,
    pub rooms: Vec<String>,
}

impl BenchmarkConfig {
    pub fn captures(&self, room_id: &str) -> bool {
        self.directory.is_some() && self.rooms.iter().any(|room| room == room_id)
    }
}

pub struct Benchmark(Mutex<Connection>);

impl Benchmark {
    pub fn open(directory: &Path, create: bool) -> Result<Self> {
        if create && !directory.exists() {
            match DirBuilder::new().mode(0o700).create(directory) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(e.into()),
            }
        }
        let meta = fs::symlink_metadata(directory)?;
        ensure!(
            meta.is_dir() && meta.permissions().mode() & 0o077 == 0,
            "benchmark directory must be private (0700) and not a symlink"
        );
        let path = directory.join("benchmark.sqlite3");
        if create && !path.exists() {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(e.into()),
            }
        }
        let meta = fs::symlink_metadata(&path)?;
        ensure!(
            meta.is_file() && meta.permissions().mode() & 0o077 == 0,
            "benchmark database must be private (0600) and not a symlink"
        );
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        conn.busy_timeout(Duration::from_millis(250))?;
        if create {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS records (
                seq INTEGER PRIMARY KEY, kind TEXT NOT NULL, room TEXT NOT NULL,
                signal_id TEXT, payload TEXT NOT NULL, UNIQUE(kind, room, signal_id)
            );",
            )?;
        }
        Ok(Self(Mutex::new(conn)))
    }

    pub fn record(
        &self,
        kind: &str,
        room: &str,
        signal_id: Option<&str>,
        data: Value,
    ) -> Result<()> {
        let payload = json!({"schema_version": 1, "kind": kind, "room_id": room,
            "recorded_at": chrono::Utc::now(), "data": data});
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("benchmark database lock poisoned"))?
            .execute(
                "INSERT INTO records(kind, room, signal_id, payload) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(kind, room, signal_id) DO NOTHING",
                params![kind, room, signal_id, payload.to_string()],
            )?;
        Ok(())
    }

    pub fn export(&self, path: &Path) -> Result<()> {
        let db = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("benchmark database lock poisoned"))?;
        let mut query = db.prepare("SELECT payload FROM records ORDER BY seq")?;
        let rows = query.query_map([], |row| row.get::<_, String>(0))?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        let mut writer = BufWriter::new(file);
        for row in rows {
            writeln!(writer, "{}", row?)?;
        }
        writer.flush()?;
        Ok(())
    }
}
