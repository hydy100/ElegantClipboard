//! SQLite owns replacement and rollback, including WAL sidecars. Never delete the
//! live database or fall back to a partial filesystem copy on failure.
use rusqlite::{backup::{Backup, StepResult}, Connection, OpenFlags};
use std::{path::Path, time::{Duration, Instant}};

pub(super) fn replace_database(staging: &Path, destination: &Path) -> Result<(), String> {
    let source = Connection::open_with_flags(staging, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let check: String = source.query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    if check != "ok" { return Err(format!("Import database integrity check: {check}")); }
    // Reject unrelated/empty SQLite files before touching the destination.
    source.prepare("SELECT id, content_type, content_hash FROM clipboard_items LIMIT 0")
        .map_err(|e| e.to_string())?;
    source.prepare("SELECT key, value FROM settings LIMIT 0")
        .map_err(|e| e.to_string())?;
    let mut target = Connection::open(destination).map_err(|e| e.to_string())?;
    target.busy_timeout(Duration::ZERO).map_err(|e| e.to_string())?;
    copy_pages(&source, &mut target, Duration::from_secs(5))
}

fn copy_pages(source: &Connection, target: &mut Connection, timeout: Duration) -> Result<(), String> {
    let backup = Backup::new(source, target).map_err(|e| e.to_string())?;
    let started = Instant::now();
    loop {
        match backup.step(256).map_err(|e| e.to_string())? {
            StepResult::Done => return Ok(()),
            StepResult::Busy | StepResult::Locked => std::thread::sleep(Duration::from_millis(50)),
            _ => {}
        }
        if started.elapsed() >= timeout {
            // Backup::drop calls sqlite3_backup_finish, rolling back unfinished work.
            return Err("Import database busy or timed out; original database retained".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn fixture() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("elegant-import-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn init(path: &Path, text: &str) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("CREATE TABLE clipboard_items(id INTEGER, content_type TEXT, content_hash TEXT); CREATE TABLE settings(key TEXT, value TEXT);").unwrap();
        conn.execute("INSERT INTO clipboard_items VALUES (1, 'text', ?1)", [text]).unwrap();
        conn
    }
    fn content(conn: &Connection) -> String {
        conn.query_row("SELECT content_hash FROM clipboard_items", [], |r| r.get(0)).unwrap()
    }
    #[test]
    fn invalid_import_preserves_original_and_staging() {
        let dir = fixture(); let live = dir.join("clipboard.db"); let stage = dir.join("clipboard.db.import");
        drop(init(&live, "original"));
        std::fs::write(&stage, b"not a database").unwrap();
        let before = std::fs::read(&live).unwrap();
        assert!(replace_database(&stage, &live).is_err());
        assert_eq!(std::fs::read(&live).unwrap(), before);
        assert!(stage.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn successful_import_handles_existing_wal_without_deleting_sidecars() {
        let dir = fixture(); let live = dir.join("clipboard.db"); let stage = dir.join("clipboard.db.import");
        let original = init(&live, "original");
        original.execute_batch("PRAGMA journal_mode=WAL; UPDATE clipboard_items SET content_hash='wal-original';").unwrap();
        drop(init(&stage, "imported"));
        replace_database(&stage, &live).unwrap();
        assert_eq!(content(&original), "imported");
        drop(original);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn locked_destination_times_out_and_can_retry() {
        let dir = fixture(); let live = dir.join("clipboard.db"); let stage = dir.join("clipboard.db.import");
        let lock = init(&live, "original"); let source = init(&stage, "imported");
        lock.execute_batch("BEGIN IMMEDIATE").unwrap();
        let mut target = Connection::open(&live).unwrap(); target.busy_timeout(Duration::ZERO).unwrap();
        assert!(copy_pages(&source, &mut target, Duration::ZERO).is_err());
        assert_eq!(content(&lock), "original");
        lock.execute_batch("ROLLBACK").unwrap();
        copy_pages(&source, &mut target, Duration::from_secs(1)).unwrap();
        assert_eq!(content(&target), "imported");
        drop((source, target, lock)); std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn interrupted_partial_backup_rolls_back_destination() {
        let source = Connection::open_in_memory().unwrap();
        source.execute_batch("CREATE TABLE large(value BLOB); INSERT INTO large VALUES(zeroblob(4000000));").unwrap();
        let mut target = Connection::open_in_memory().unwrap();
        target.execute_batch("CREATE TABLE original(value); INSERT INTO original VALUES(7);").unwrap();
        assert!(copy_pages(&source, &mut target, Duration::ZERO).is_err());
        assert_eq!(target.query_row("SELECT value FROM original", [], |r| r.get::<_, i32>(0)).unwrap(), 7);
    }
}
