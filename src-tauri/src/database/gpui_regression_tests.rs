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
fn gpui_list_reports_corrupt_row_without_hiding_history() {
    let f = Fixture::new();
    f.insert("valid");
    let bad = f.insert("invalid");
    f.db().write_connection().lock().execute("UPDATE clipboard_items SET byte_size = X'01', image_path = 'fixture.png' WHERE id = ?", [bad]).unwrap();
    let repo = ClipboardRepository::new(f.db());
    assert!(repo.list(QueryOptions::default()).is_err());
    assert!(repo.query_media_items().is_err());
    assert_eq!(repo.count(QueryOptions::default()).unwrap(), 2);
}

#[test]
fn gpui_media_reference_reads_reject_partial_results() {
    let f = Fixture::new();
    let id = f.insert("media");
    f.db().write_connection().lock().execute("UPDATE clipboard_items SET image_path = X'01', source_app_icon = X'01' WHERE id = ?", [id]).unwrap();
    let repo = ClipboardRepository::new(f.db());
    assert!(repo.get_all_image_paths().is_err());
    assert!(repo.get_all_icon_paths().is_err());
    assert!(repo.get_clearable_image_paths(None).is_err());
}

#[test]
fn gpui_orphan_cleanup_aborts_on_unreadable_references() {
    let f = Fixture::new();
    let id = f.insert("unreadable media");
    f.db().write_connection().lock().execute("UPDATE clipboard_items SET image_path = X'01', source_app_icon = X'01' WHERE id = ?", [id]).unwrap();
    for name in ["images", "icons"] {
        std::fs::create_dir(f.dir.join(name)).unwrap();
        std::fs::write(f.dir.join(name).join("keep.png"), b"fixture").unwrap();
    }
    crate::clipboard::cleanup_orphan_files(f.db(), &f.dir);
    assert!(f.dir.join("images/keep.png").exists());
    assert!(f.dir.join("icons/keep.png").exists());
}

#[test]
fn gpui_batch_delete_rejects_unreadable_candidates_without_deleting_rows() {
    let f = Fixture::new();
    let a = f.insert("first");
    let b = f.insert("second");
    f.db().write_connection().lock().execute("UPDATE clipboard_items SET image_path = X'01' WHERE id = ?", [b]).unwrap();
    let repo = ClipboardRepository::new(f.db());
    assert!(repo.batch_delete(&[a, b]).is_err());
    assert_eq!(repo.count(QueryOptions::default()).unwrap(), 2);
}

#[test]
fn gpui_settings_and_tag_export_reject_partial_results() {
    let f = Fixture::new();
    let conn = f.db().write_connection();
    conn.lock().execute_batch("INSERT INTO settings(key, value) VALUES ('broken', X'01'); INSERT INTO tags(name) VALUES (X'01');").unwrap();
    let settings = SettingsRepository::new(f.db());
    assert!(settings.get_all().is_err());
    assert!(settings.get_multiple(&["broken", "hotkey"]).is_err());
    let tags = TagRepository::new(f.db());
    assert!(tags.list_with_count().is_err());
    assert!(tags.export_tags_sync_data().is_err());
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

#[test]
fn gpui_batch_delete_keeps_shared_images_and_deduplicates_cleanup_paths() {
    let f = Fixture::new();
    let a = f.insert("a"); let b = f.insert("b"); let keep = f.insert("keep");
    f.db().write_connection().lock().execute_batch("UPDATE clipboard_items SET image_path='shared.png';").unwrap();
    let repo = ClipboardRepository::new(f.db());
    assert_eq!(repo.batch_delete(&[a, b]).unwrap(), (2, vec![]));
    let c = f.insert("c");
    f.db().write_connection().lock().execute("UPDATE clipboard_items SET image_path='shared.png' WHERE id=?", [c]).unwrap();
    assert_eq!(repo.batch_delete(&[keep, c]).unwrap(), (2, vec!["shared.png".into()]));
}

#[test]
fn gpui_sync_export_never_publishes_partial_settings_or_tags() {
    for corrupt in ["INSERT INTO settings(key, value) VALUES ('broken', X'01')", "INSERT INTO tags(name) VALUES (X'01')"] {
        let f = Fixture::new();
        f.insert("valid history");
        f.db().write_connection().lock().execute(corrupt, []).unwrap();
        let result = crate::webdav::export_sync_data_to_file(f.db(), &f.dir, &crate::webdav::SyncOptions::default());
        assert!(result.is_err(), "export succeeded with incomplete settings/tags");
        assert!(!std::fs::read_dir(&f.dir).unwrap().flatten().any(|entry| entry.path().extension().is_some_and(|ext| ext == "zip")), "failed archive was not removed");
    }
}

#[test]
fn gpui_valid_sync_export_keeps_flags_tags_and_compact_schema() {
    use std::io::Read;
    let f = Fixture::new();
    let id = f.insert("roundtrip");
    let repo = ClipboardRepository::new(f.db());
    repo.toggle_favorite(id).unwrap();
    repo.toggle_pin(id).unwrap();
    let tags = TagRepository::new(f.db());
    let tag = tags.create("keep").unwrap(); tags.add_tag_to_item(id, tag.id).unwrap();
    let (path, _) = crate::webdav::export_sync_data_to_file(f.db(), &f.dir, &crate::webdav::SyncOptions::default()).unwrap();
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut json = String::new(); archive.by_name("items.json").unwrap().read_to_string(&mut json).unwrap();
    let items: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(items[0]["is_favorite"], true); assert_eq!(items[0]["is_pinned"], true);
    json.clear(); archive.by_name("tags.json").unwrap().read_to_string(&mut json).unwrap();
    let data: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(data["tags"][0]["name"], "keep");
    assert_eq!(data["tags"][0]["associations"][0]["content_hash"], "roundtrip");
    assert!(data.get("associations").is_none());
}
