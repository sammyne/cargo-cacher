use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread;

use super::CargoRequest;

use rusqlite;

pub struct Database {
    conn: rusqlite::Connection,
}

#[derive(Debug)]
pub struct Crate {
    id: i32,
    name: String,
}

#[derive(Debug)]
pub struct Statistics {
    pub downloads: i64,
    pub hits: i64,
    pub misses: i64,
    pub bandwidth_saved: i64,
}

impl Statistics {
    pub fn as_json(&self) -> String {
        format!(r#"{{"downloads": {}, "hits": {}, "misses": {}, "bandwidth_saved": {}}}"#,
                self.downloads,
                self.hits,
                self.misses,
                self.bandwidth_saved)
    }
}

impl Database {
    pub fn new<T: Into<String>>(connection_string: Option<T>) -> Database {

        let connection_string: String = if let Some(s) = connection_string {
            s.into()
        } else {
            "file::memory:?cache=shared".to_string()
            // "database.sqlite".into()
        };
        let conn = rusqlite::SqliteConnection::open(&connection_string).unwrap();
        conn.execute("
            CREATE TABLE IF NOT EXISTS crates (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT
            );",
                     &[])
            .unwrap();
        conn.execute("
             CREATE TABLE IF NOT EXISTS crate_versions (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 version TEXT,
                 crate_id INTEGER
             );",
                     &[])
            .unwrap();
        conn.execute("
             CREATE TABLE IF NOT EXISTS downloads (
                 version_id INTEGER,
                 time TIMESTAMP,
                 hit BOOLEAN,
                 size BIGINT
             );",
                     &[])
            .unwrap();

        conn.execute("
            CREATE UNIQUE INDEX IF NOT EXISTS unique_crate_names
            ON crates (name)",
                     &[])
            .unwrap();

        conn.execute("
            CREATE UNIQUE INDEX IF NOT EXISTS unique_crate_versions
            ON crate_versions (crate_id, version)",
                     &[])
            .unwrap();
        Database { conn: conn }
    }

    pub fn stats(&self) -> Statistics {
        let downloads = self.downloads("24 hours");
        let hits = self.hits("24 hours");
        let misses = downloads - hits;
        let bandwidth_saved = self.bandwidth_saved("24 hours");
        Statistics {
            downloads: downloads as i64,
            hits: hits as i64,
            misses: misses as i64,
            bandwidth_saved: bandwidth_saved as i64,
        }
    }

    pub fn downloads<T: Into<String>>(&self, time: T) -> i32 {
        let mut stmt = self.conn
            .prepare("SELECT count(*) FROM downloads WHERE time > date('now') - $1")
            .unwrap();
        let rows = match stmt.query_map(&[&time.into()], |row| row.get(0)) {
            Ok(s) => s,
            _ => return 0,
        };
        for record in rows {
            if let Ok(count) = record {
                return count;
            }
        }
        0
    }

    pub fn hits<T: Into<String>>(&self, time: T) -> i32 {
        let mut stmt = self.conn
            .prepare("SELECT count(*) FROM downloads WHERE time > date('now') - $1 AND hit = 1")
            .unwrap();
        let rows = match stmt.query_map(&[&time.into()], |row| row.get(0)) {
            Ok(s) => s,
            _ => return 0,
        };
        for record in rows {
            if let Ok(count) = record {
                return count;
            }
        }
        0
    }

    pub fn bandwidth_saved<T: Into<String>>(&self, time: T) -> i64 {
        let mut stmt = self.conn
            .prepare("SELECT COALESCE(sum(size), 0) FROM downloads WHERE time > date('now') - $1 \
                      AND hit = 1")
            .unwrap();
        let rows = match stmt.query_map(&[&time.into()], |row| row.get(0)) {
            Ok(s) => s,
            _ => return 0,
        };
        for record in rows {
            if let Ok(count) = record {
                return count;
            }
        }
        0
    }

    pub fn crates(&self) -> Result<Vec<Crate>, rusqlite::Error> {
        let mut stmt = self.conn.prepare("SELECT id, name FROM crates").unwrap();
        let rows = stmt.query_map(&[], |row| {
                Crate {
                    id: row.get(0),
                    name: row.get(1),
                }
            })?;
        let mut crates = Vec::new();
        for record in rows {
            crates.push(record?);
        }
        Ok(crates)
    }

    fn crate_id<T: Into<String>>(&self, name: T) -> Option<i32> {
        let mut stmt = self.conn.prepare("SELECT id FROM crates WHERE name = $1").unwrap();
        let rows = stmt.query_map(&[&name.into()], |row| row.get(0)).unwrap();
        for record in rows {
            if let Ok(id) = record {
                return Some(id);
            }
        }
        return None;
    }

    fn version_id<T: Into<String>>(&self, crate_id: i32, version: T) -> Option<i32> {
        let mut stmt = self.conn
            .prepare("SELECT id
            FROM crate_versions
            WHERE crate_id = $1 \
                      AND version = $2")
            .unwrap();
        let rows = stmt.query_map(&[&crate_id, &version.into()], |row| row.get(0)).unwrap();
        for record in rows {
            if let Ok(id) = record {
                return Some(id);
            }
        }
        return None;
    }

    pub fn add_request<T: Into<String>, S: Into<String>>(&self,
                                                         crate_name: T,
                                                         crate_version: S,
                                                         hit: bool,
                                                         size: i64)
                                                         -> Result<(), rusqlite::Error> {
        let crate_name = crate_name.into();
        let crate_version = crate_version.into();
        let _ = self.conn
            .execute("INSERT OR IGNORE INTO crates (name) VALUES ($1)",
                     &[&crate_name])
            .unwrap();
        let crate_id = self.crate_id(crate_name).unwrap();
        let _ = self.conn
            .execute("INSERT OR IGNORE INTO crate_versions (crate_id, version) VALUES ($1, $2)",
                     &[&crate_id, &crate_version])
            .unwrap();
        let version_id = self.version_id(crate_id, crate_version).unwrap();

        info!("Version ID: {}", version_id);
        let _ = self.conn
            .execute("INSERT INTO downloads (version_id, time, hit, size) VALUES ($1, \
                      date('now'), $2, $3)",
                     &[&version_id, &hit, &size]);
        Ok(())

    }
}


pub fn stat_collector() -> SyncSender<CargoRequest> {
    let (sender, receiver) = sync_channel::<CargoRequest>(10);
    let db = Database::new(None::<&str>);
    thread::spawn(move || loop {
        if let Ok(req) = receiver.recv() {
            info!("Logging a crate request to sqlite: {:?}", req);
            let _ = db.add_request(req.name, req.version, req.hit, req.size).unwrap();
        } else {
            break;
        }
    });
    sender
}
