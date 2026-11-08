//! Regression cases adapted to the personal Tauri database, not the GPUI schema.
use super::*;

struct Fixture {
    db: Option<Database>,
    dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("elegant-gpui-review-{}", uuid::Uuid::new_v4()));
        Self { db: Some(Database::new(dir.join("clipboard.db")).unwrap()), dir }
    }
    fn db(&self) -> &Database { self.db.as_ref().unwrap() }
    fn insert(&self, text: &str) -> i64 {
        ClipboardRepository::new(self.db()).insert(NewClipboardItem {
            text_content: Some(text.into()), content_hash: text.into(), semantic_hash: text.into(),
            preview: Some(text.into()), ..NewClipboardItem::default()
        }).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.db.take();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn gpui_first_open_completes_post_schema_migrations() {
    let f = Fixture::new();
    for key in ["_migration_url_content_type", "_migration_backfill_semantic_hash", "_migration_video_content_type"] {
        assert_eq!(SettingsRepository::new(f.db()).get(key).unwrap().as_deref(), Some("done"), "{key}");
    }
}

#[test]
fn gpui_url_marker_failure_rolls_back_all_data_updates() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT);
        CREATE TABLE clipboard_items(id INTEGER PRIMARY KEY, content_type TEXT, text_content TEXT, content_hash TEXT, semantic_hash TEXT);
        INSERT INTO clipboard_items VALUES (1, 'text', 'https://example.com', 'raw', '');
        CREATE TRIGGER reject_marker BEFORE INSERT ON settings BEGIN SELECT RAISE(ABORT, 'injected marker failure'); END;").unwrap();
    assert!(Database::migrate_url_content_type(&conn).is_err());
    let row: (String, String) = conn.query_row("SELECT content_type, semantic_hash FROM clipboard_items", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(row, ("text".into(), "".into()));
}

#[test]
fn gpui_reopen_never_inserts_probe_rows_or_deletes_real_sentinel_hashes() {
    let mut f = Fixture::new();
    let id = f.insert("__test__");
    let before: i64 = f.db().write_connection().lock().query_row(
        "SELECT seq FROM sqlite_sequence WHERE name='clipboard_items'", [], |r| r.get(0)
    ).unwrap();
    f.db.take();
    let reopened = Database::new(f.dir.join("clipboard.db")).unwrap();
    assert!(ClipboardRepository::new(&reopened).get_by_id(id).unwrap().is_some());
    let after: i64 = reopened.write_connection().lock().query_row(
        "SELECT seq FROM sqlite_sequence WHERE name='clipboard_items'", [], |r| r.get(0)
    ).unwrap();
    assert_eq!(before, after);
}

#[test]
fn gpui_legacy_without_settings_preserves_tags_flags_and_backfills_once() {
    let f = Fixture::new();
    let path = f.dir.join("legacy.db");
    {
        let conn = Connection::open(&path).unwrap();
        let schema = SCHEMA_SQL.replace("content_type TEXT NOT NULL,", "content_type TEXT NOT NULL CHECK(content_type IN ('text','files','image','html','rtf')),");
        conn.execute_batch(&schema).unwrap();
        conn.execute_batch("DROP TABLE settings;
            INSERT INTO clipboard_items(id, content_type, text_content, content_hash, semantic_hash, is_favorite, is_pinned)
                VALUES (1, 'text', 'https://example.com', 'url-hash', '', 1, 1);
            INSERT INTO clipboard_items(id, content_type, file_paths, content_hash, semantic_hash)
                VALUES (2, 'files', '[\"movie.MP4\"]', 'video-hash', 'video-hash');
            INSERT INTO tags(id, name) VALUES (1, 'keep');
            INSERT INTO item_tags(item_id, tag_id, sort_order) VALUES (1, 1, 7);").unwrap();
    }
    let db = Database::new(path).unwrap();
    let repo = ClipboardRepository::new(&db);
    let url = repo.get_by_id(1).unwrap().unwrap();
    assert_eq!(url.content_type, "url");
    assert!(url.is_pinned && url.is_favorite);
    assert_eq!(repo.get_by_id(2).unwrap().unwrap().content_type, "video");
    assert_eq!(TagRepository::new(&db).get_tags_for_item(1).unwrap()[0].name, "keep");
    assert_eq!(db.write_connection().lock().query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    for key in ["_migration_url_content_type", "_migration_video_content_type", "_migration_backfill_semantic_hash"] {
        assert_eq!(SettingsRepository::new(&db).get(key).unwrap().as_deref(), Some("done"));
    }
}

#[test]
fn gpui_constraint_rebuild_and_marker_failure_restore_original_table_and_tags() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    let schema = SCHEMA_SQL.replace("content_type TEXT NOT NULL,", "content_type TEXT NOT NULL CHECK(content_type IN ('text','files','image','html','rtf')),");
    conn.execute_batch(&schema).unwrap();
    conn.execute_batch("INSERT INTO clipboard_items(id, content_type, text_content, content_hash, semantic_hash)
        VALUES (1, 'text', 'https://example.com', 'raw', '');
        INSERT INTO tags(id, name) VALUES (1, 'keep'); INSERT INTO item_tags VALUES (1, 1, 'now', 0);
        CREATE TRIGGER reject_marker BEFORE INSERT ON settings BEGIN SELECT RAISE(ABORT, 'injected failure'); END;").unwrap();
    assert!(Database::migrate_url_content_type(&conn).is_err());
    let sql: String = conn.query_row("SELECT sql FROM sqlite_master WHERE name='clipboard_items'", [], |r| r.get(0)).unwrap();
    assert!(sql.contains("CHECK"));
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM item_tags", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(conn.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(conn.query_row("SELECT content_type FROM clipboard_items", [], |r| r.get::<_, String>(0)).unwrap(), "text");
    conn.execute_batch("DROP TRIGGER reject_marker;").unwrap();
    Database::migrate_url_content_type(&conn).unwrap();
    assert_eq!(conn.query_row("SELECT content_type FROM clipboard_items", [], |r| r.get::<_, String>(0)).unwrap(), "url");
}
