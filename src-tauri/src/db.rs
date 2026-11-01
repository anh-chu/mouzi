use chrono::{DateTime, Utc};
use once_cell::sync::OnceCell;
use rusqlite::{params, Connection, Result as SqliteResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Folder modes
// ---------------------------------------------------------------------------

pub const FOLDER_MODE_SILENT: &str = "silent";
pub const FOLDER_MODE_MANUAL: &str = "manual";
pub const FOLDER_MODE_PAUSED: &str = "paused";

pub const FOLDER_MODES: &[&str] = &[
    FOLDER_MODE_SILENT,
    FOLDER_MODE_MANUAL,
    FOLDER_MODE_PAUSED,
    "suggest",
];

pub fn is_folder_auto_mode(mode: &str) -> bool {
    mode == FOLDER_MODE_SILENT
}

pub fn is_folder_manual_mode(mode: &str) -> bool {
    mode == FOLDER_MODE_MANUAL || mode == "suggest"
}

pub fn is_folder_paused_mode(mode: &str) -> bool {
    mode == FOLDER_MODE_PAUSED
}

pub fn is_valid_folder_mode(mode: &str) -> bool {
    FOLDER_MODES.contains(&mode)
}

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleOptions {
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub modified_after: Option<String>,
    pub modified_before: Option<String>,
    pub rename_template: String,
}

impl Default for RuleOptions {
    fn default() -> Self {
        Self {
            min_size: None,
            max_size: None,
            modified_after: None,
            modified_before: None,
            rename_template: "{stem}.{extension}".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub id: Option<i64>,
    pub name: String,
    pub priority: i32,
    pub enabled: bool,
    pub extensions: Vec<String>,
    pub pattern: Option<String>,
    pub destination: String,
    pub action: String, // "move", "rename", "delete", "ignore"
    pub folder_id: i64,
    #[serde(default)]
    pub notification_message: Option<String>,
    #[serde(default)]
    pub normalize_extensions: bool,
    #[serde(default = "default_extension_mappings")]
    pub extension_mappings: String,
    #[serde(default)]
    pub options: RuleOptions,
}

fn default_extension_mappings() -> String {
    "jpeg:jpg".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchedFolder {
    pub id: Option<i64>,
    pub path: String,
    pub enabled: bool,
    /// One of: "silent" (real-time auto-organize), "manual" (collect only),
    /// "paused" (do not watch).
    pub mode: String,
    pub only_new: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionLog {
    pub id: Option<i64>,
    pub timestamp: DateTime<Utc>,
    pub source_path: String,
    pub destination_path: Option<String>,
    pub action: String,
    pub file_name: String,
    pub file_type: String,
    pub undone: bool,
    pub run_id: Option<String>,
    pub trigger: String,
    pub file_extension: String,
    pub file_size: u64,
    pub fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub id: Option<i64>,
    pub language: String,
    pub theme: String,
    pub telemetry_enabled: bool,
    pub first_run: bool,
    pub autostart: bool,
    pub grace_period_seconds: i64,
    pub lock_check_enabled: bool,
    pub auto_update_enabled: bool,
    pub schedule_enabled: bool,
    pub schedule_times_per_day: i64,
    pub schedule_time_1: Option<String>,
    pub schedule_time_2: Option<String>,
    pub schedule_time_3: Option<String>,
    pub schedule_time_4: Option<String>,
    pub ai_enabled: bool,
    pub ai_model: String,
    pub ai_auto_move: bool,
    pub ai_delete_folder: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleSettings {
    pub schedule_enabled: bool,
    pub schedule_times_per_day: i64,
    pub schedule_time_1: Option<String>,
    pub schedule_time_2: Option<String>,
    pub schedule_time_3: Option<String>,
    pub schedule_time_4: Option<String>,
}

static DB: OnceCell<Arc<Mutex<Connection>>> = OnceCell::new();

pub fn init_db(app_dir: PathBuf) -> SqliteResult<()> {
    let db_path = app_dir.join("mouzi.db");
    let conn = Connection::open(db_path)?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS watched_folders (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL UNIQUE,
            enabled INTEGER NOT NULL DEFAULT 1,
            mode TEXT NOT NULL DEFAULT 'silent'
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS rules (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            priority INTEGER NOT NULL DEFAULT 0,
            enabled INTEGER NOT NULL DEFAULT 1,
            extensions TEXT NOT NULL,
            pattern TEXT,
            destination TEXT NOT NULL,
            action TEXT NOT NULL DEFAULT 'move',
            folder_id INTEGER NOT NULL DEFAULT 0,
            notification_message TEXT,
            normalize_extensions INTEGER NOT NULL DEFAULT 0,
            extension_mappings TEXT NOT NULL DEFAULT 'jpeg:jpg'
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS action_logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL,
            source_path TEXT NOT NULL,
            destination_path TEXT,
            action TEXT NOT NULL,
            file_name TEXT NOT NULL,
            file_type TEXT NOT NULL,
            undone INTEGER NOT NULL DEFAULT 0
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS settings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            language TEXT NOT NULL DEFAULT 'en',
            theme TEXT NOT NULL DEFAULT 'system',
            telemetry_enabled INTEGER NOT NULL DEFAULT 0,
            first_run INTEGER NOT NULL DEFAULT 1,
            autostart INTEGER NOT NULL DEFAULT 1
        )",
        [],
    )?;

    // Migration: add missing columns
    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info(settings)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !cols.iter().any(|c| c == "autostart") {
        conn.execute(
            "ALTER TABLE settings ADD COLUMN autostart INTEGER NOT NULL DEFAULT 1",
            [],
        )?;
    }
    if !cols.iter().any(|c| c == "grace_period_seconds") {
        conn.execute(
            "ALTER TABLE settings ADD COLUMN grace_period_seconds INTEGER NOT NULL DEFAULT 300",
            [],
        )?;
    }
    if !cols.iter().any(|c| c == "lock_check_enabled") {
        conn.execute(
            "ALTER TABLE settings ADD COLUMN lock_check_enabled INTEGER NOT NULL DEFAULT 1",
            [],
        )?;
    }
    if !cols.iter().any(|c| c == "auto_update_enabled") {
        conn.execute(
            "ALTER TABLE settings ADD COLUMN auto_update_enabled INTEGER NOT NULL DEFAULT 1",
            [],
        )?;
    }
    if !cols.iter().any(|c| c == "schedule_enabled") {
        conn.execute(
            "ALTER TABLE settings ADD COLUMN schedule_enabled INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !cols.iter().any(|c| c == "schedule_times_per_day") {
        conn.execute(
            "ALTER TABLE settings ADD COLUMN schedule_times_per_day INTEGER NOT NULL DEFAULT 1",
            [],
        )?;
    }
    if !cols.iter().any(|c| c == "schedule_time_1") {
        conn.execute("ALTER TABLE settings ADD COLUMN schedule_time_1 TEXT", [])?;
    }
    if !cols.iter().any(|c| c == "schedule_time_2") {
        conn.execute("ALTER TABLE settings ADD COLUMN schedule_time_2 TEXT", [])?;
    }
    if !cols.iter().any(|c| c == "schedule_time_3") {
        conn.execute("ALTER TABLE settings ADD COLUMN schedule_time_3 TEXT", [])?;
    }
    if !cols.iter().any(|c| c == "schedule_time_4") {
        conn.execute("ALTER TABLE settings ADD COLUMN schedule_time_4 TEXT", [])?;
    }
    if !cols.iter().any(|c| c == "ai_enabled") {
        conn.execute(
            "ALTER TABLE settings ADD COLUMN ai_enabled INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !cols.iter().any(|c| c == "ai_api_key") {
        conn.execute(
            "ALTER TABLE settings ADD COLUMN ai_api_key TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    if !cols.iter().any(|c| c == "ai_model") {
        conn.execute(
            "ALTER TABLE settings ADD COLUMN ai_model TEXT NOT NULL DEFAULT 'typesafe/jev-1.14'",
            [],
        )?;
    }

    if !cols.iter().any(|c| c == "ai_auto_move") {
        conn.execute("ALTER TABLE settings ADD COLUMN ai_auto_move INTEGER NOT NULL DEFAULT 0", [])?;
    }
    if !cols.iter().any(|c| c == "ai_delete_folder") {
        conn.execute("ALTER TABLE settings ADD COLUMN ai_delete_folder TEXT NOT NULL DEFAULT ''", [])?;
    }

    let rule_cols: Vec<String> = conn
        .prepare("PRAGMA table_info(rules)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !rule_cols.iter().any(|c| c == "notification_message") {
        conn.execute("ALTER TABLE rules ADD COLUMN notification_message TEXT", [])?;
    }
    if !rule_cols.iter().any(|c| c == "normalize_extensions") {
        conn.execute(
            "ALTER TABLE rules ADD COLUMN normalize_extensions INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !rule_cols.iter().any(|c| c == "extension_mappings") {
        conn.execute(
            "ALTER TABLE rules ADD COLUMN extension_mappings TEXT NOT NULL DEFAULT 'jpeg:jpg'",
            [],
        )?;
    }
    migrate_beta_schema(&conn)?;
    // Insert default settings if empty
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))?;

    if count == 0 {
        conn.execute(
            "INSERT INTO settings (language, theme, telemetry_enabled, first_run, autostart) VALUES ('en', 'system', 0, 1, 1)",
            [],
        )?;
    }

    if env!("CARGO_PKG_VERSION").contains("beta") && count == 0 {
        conn.execute("UPDATE settings SET autostart=0, auto_update_enabled=0", [])?;
    }
    DB.set(Arc::new(Mutex::new(conn)))
        .map_err(|_| rusqlite::Error::ExecuteReturnedResults)?;

    Ok(())
}

pub fn get_db() -> Arc<Mutex<Connection>> {
    DB.get().expect("Database not initialized").clone()
}

pub fn normalize_extensions(values: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    for value in values.iter().flat_map(|v| v.split(',')) {
        let value = value.trim().trim_start_matches('.').to_lowercase();
        if !value.is_empty() && !result.contains(&value) {
            result.push(value);
        }
    }
    result
}

fn migrate_beta_schema(conn: &Connection) -> SqliteResult<()> {
    for (table, name, definition) in [
        ("rules", "options", "TEXT NOT NULL DEFAULT '{}'"),
        ("watched_folders", "only_new", "INTEGER NOT NULL DEFAULT 0"),
        ("action_logs", "run_id", "TEXT"),
        ("action_logs", "trigger", "TEXT NOT NULL DEFAULT 'legacy'"),
        ("action_logs", "file_extension", "TEXT NOT NULL DEFAULT ''"),
        ("action_logs", "file_size", "INTEGER NOT NULL DEFAULT 0"),
        ("action_logs", "fingerprint", "TEXT"),
    ] {
        let columns = conn
            .prepare(&format!("PRAGMA table_info({table})"))?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<SqliteResult<Vec<_>>>()?;
        if !columns.iter().any(|column| column == name) {
            conn.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {name} {definition}"),
                [],
            )?;
        }
    }
    conn.execute_batch("CREATE TABLE IF NOT EXISTS folder_baseline (folder_id INTEGER NOT NULL, path TEXT NOT NULL, PRIMARY KEY(folder_id,path)); CREATE INDEX IF NOT EXISTS logs_run ON action_logs(run_id);")?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS processed_files (path TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, restored INTEGER NOT NULL DEFAULT 0)")?;
    let rows = conn
        .prepare("SELECT id, extensions FROM rules")?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .collect::<SqliteResult<Vec<_>>>()?;
    for (id, extensions) in rows {
        conn.execute(
            "UPDATE rules SET extensions=?1 WHERE id=?2",
            params![normalize_extensions(&[extensions]).join(","), id],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn upgrade_preserves_existing_rows_and_is_repeatable() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE rules(id INTEGER PRIMARY KEY, extensions TEXT, destination TEXT, enabled INTEGER);
            CREATE TABLE watched_folders(id INTEGER PRIMARY KEY, path TEXT, mode TEXT);
            CREATE TABLE action_logs(id INTEGER PRIMARY KEY, source_path TEXT, destination_path TEXT, undone INTEGER);
            CREATE TABLE settings(language TEXT, autostart INTEGER, auto_update_enabled INTEGER);
            INSERT INTO rules VALUES(7, 'exe, msi,', 'Installers', 0);
            INSERT INTO watched_folders VALUES(3, '/example/downloads', 'paused');
            INSERT INTO action_logs VALUES(42, '/example/a', '/example/b', 0);
            INSERT INTO settings VALUES('pl', 0, 0);").unwrap();
        migrate_beta_schema(&conn).unwrap();
        migrate_beta_schema(&conn).unwrap();
        let rule: (String, String, i64, String) = conn.query_row("SELECT extensions,destination,enabled,options FROM rules WHERE id=7", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(rule, ("exe,msi".into(), "Installers".into(), 0, "{}".into()));
        let folder: (String, i64) = conn.query_row("SELECT mode,only_new FROM watched_folders WHERE id=3", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(folder, ("paused".into(), 0));
        let log: (String, String, Option<String>) = conn.query_row("SELECT destination_path,trigger,fingerprint FROM action_logs WHERE id=42", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(log, ("/example/b".into(), "legacy".into(), None));
        let settings: (String,i64,i64) = conn.query_row("SELECT language,autostart,auto_update_enabled FROM settings", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(settings, ("pl".into(),0,0));
    }
}

pub fn set_only_new(id: i64, enabled: bool) -> Result<(), String> {
    let folder = get_watched_folders()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|f| f.id == Some(id))
        .ok_or("Folder not found")?;
    if folder.only_new == enabled {
        return Ok(());
    }
    let mut paths = Vec::new();
    if enabled {
        for entry in std::fs::read_dir(&folder.path).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.path().is_file() {
                paths.push(entry.path().to_string_lossy().to_string());
            }
        }
    }
    let db = get_db();
    let mut conn = db.lock().unwrap();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM folder_baseline WHERE folder_id=?1", [id])
        .map_err(|e| e.to_string())?;
    for path in paths {
        tx.execute(
            "INSERT INTO folder_baseline(folder_id,path) VALUES (?1,?2)",
            params![id, path],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.execute(
        "UPDATE watched_folders SET only_new=?1 WHERE id=?2",
        params![enabled, id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

pub fn is_baseline_file(path: &std::path::Path) -> bool {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.query_row("SELECT EXISTS(SELECT 1 FROM folder_baseline b JOIN watched_folders f ON f.id=b.folder_id WHERE f.only_new=1 AND b.path=?1)", [path.to_string_lossy().as_ref()], |r| r.get(0)).unwrap_or(true)
}

pub fn forget_removed_baseline(path: &std::path::Path) {
    if !path.exists() {
        let _ = get_db().lock().unwrap().execute(
            "DELETE FROM folder_baseline WHERE path=?1",
            [path.to_string_lossy().as_ref()],
        );
    }
}

pub fn migrate_rules_to_relative() -> SqliteResult<()> {
    let folders = get_watched_folders()?;
    let db = get_db();
    let conn = db.lock().unwrap();
    for folder in folders {
        let folder_norm = folder.path.trim_end_matches('/').trim_end_matches('\\');
        if folder_norm.is_empty() {
            continue;
        }
        let mut stmt =
            conn.prepare("SELECT id, destination FROM rules WHERE destination LIKE ?1")?;
        let rows: Vec<(i64, String)> = stmt
            .query_map([format!("{}%", folder_norm)], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<SqliteResult<Vec<_>>>()?;
        for (id, dest) in rows {
            let relative = if dest.starts_with(&folder_norm) {
                dest[folder_norm.len()..]
                    .trim_start_matches('/')
                    .trim_start_matches('\\')
                    .to_string()
            } else {
                dest.clone()
            };
            if !relative.is_empty() && relative != dest {
                conn.execute(
                    "UPDATE rules SET destination = ?1 WHERE id = ?2",
                    params![relative, id],
                )?;
            }
        }
    }
    Ok(())
}

pub fn insert_default_rules(_folder_path: &str) -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();

    // Only insert defaults if no rules exist yet
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM rules", [], |row| row.get(0))?;
    if count > 0 {
        return Ok(());
    }

    let defaults = vec![
        (
            "Images",
            1,
            vec![
                "jpg", "jpeg", "png", "gif", "webp", "bmp", "svg", "ico", "heic", "heif",
            ],
            "Images",
        ),
        (
            "Documents",
            2,
            vec![
                "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "rtf", "odt",
            ],
            "Documents",
        ),
        (
            "Archives",
            3,
            vec!["zip", "rar", "7z", "tar", "gz", "bz2", "xz"],
            "Archives",
        ),
        (
            "Installers",
            4,
            if cfg!(target_os = "macos") {
                vec!["exe", "msi", "msix", "appx", "dmg", "pkg"]
            } else {
                vec!["exe", "msi", "msix", "appx"]
            },
            "Installers",
        ),
        (
            "Music",
            5,
            vec!["mp3", "wav", "flac", "aac", "ogg", "wma", "m4a"],
            "Music",
        ),
        (
            "Videos",
            6,
            vec!["mp4", "avi", "mkv", "mov", "wmv", "flv", "webm"],
            "Videos",
        ),
        ("Others", 99, vec!["*"], "Others"),
    ];

    for (name, priority, exts, dest) in defaults {
        let extensions = exts.join(",");
        let destination = dest.to_string();
        conn.execute(
            "INSERT INTO rules (name, priority, extensions, destination, action, folder_id) VALUES (?1, ?2, ?3, ?4, 'move', 0)",
            params![name, priority, extensions, destination],
        )?;
    }

    Ok(())
}

pub fn get_rules() -> SqliteResult<Vec<Rule>> {
    let db = get_db();
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id, name, priority, enabled, extensions, pattern, destination, action, folder_id, notification_message, normalize_extensions, extension_mappings, options FROM rules ORDER BY priority, id"
    )?;

    let rules = stmt
        .query_map([], |row| {
            let exts_str: String = row.get(4)?;
            Ok(Rule {
                id: row.get(0)?,
                name: row.get(1)?,
                priority: row.get(2)?,
                enabled: row.get::<_, i32>(3)? != 0,
                extensions: normalize_extensions(&[exts_str]),
                pattern: row.get(5)?,
                destination: row.get(6)?,
                action: row.get(7)?,
                folder_id: row.get(8)?,
                notification_message: row.get(9)?,
                normalize_extensions: row.get::<_, i32>(10)? != 0,
                extension_mappings: row.get(11)?,
                options: serde_json::from_str(&row.get::<_, String>(12)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        12,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
            })
        })?
        .collect::<SqliteResult<Vec<_>>>()?;

    Ok(rules)
}

pub fn add_rule(rule: &Rule) -> SqliteResult<i64> {
    crate::rules::validate_rule(rule).map_err(rusqlite::Error::InvalidParameterName)?;
    let db = get_db();
    let conn = db.lock().unwrap();
    insert_rule(&conn, rule)
}

fn insert_rule(conn: &Connection, rule: &Rule) -> SqliteResult<i64> {
    let exts = normalize_extensions(&rule.extensions).join(",");
    conn.execute(
        "INSERT INTO rules (name, priority, enabled, extensions, pattern, destination, action, folder_id, notification_message, normalize_extensions, extension_mappings, options) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![rule.name, rule.priority, rule.enabled as i32, exts, rule.pattern, rule.destination, rule.action, rule.folder_id, rule.notification_message, rule.normalize_extensions as i32, rule.extension_mappings, serde_json::to_string(&rule.options).unwrap()],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn import_rules(rules: &[Rule], replace: bool) -> Result<usize, String> {
    for rule in rules {
        crate::rules::validate_rule(rule)?;
    }
    let db = get_db();
    let mut conn = db.lock().unwrap();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    if replace {
        tx.execute("DELETE FROM rules", [])
            .map_err(|e| e.to_string())?;
    }
    for rule in rules {
        insert_rule(&tx, rule).map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(rules.len())
}

pub fn update_rule(rule: &Rule) -> SqliteResult<()> {
    crate::rules::validate_rule(rule).map_err(rusqlite::Error::InvalidParameterName)?;
    let db = get_db();
    let conn = db.lock().unwrap();
    let exts = normalize_extensions(&rule.extensions).join(",");
    conn.execute(
        "UPDATE rules SET name=?1, priority=?2, enabled=?3, extensions=?4, pattern=?5, destination=?6, action=?7, folder_id=?8, notification_message=?9, normalize_extensions=?10, extension_mappings=?11, options=?12 WHERE id=?13",
        params![rule.name, rule.priority, rule.enabled as i32, exts, rule.pattern, rule.destination, rule.action, rule.folder_id, rule.notification_message, rule.normalize_extensions as i32, rule.extension_mappings, serde_json::to_string(&rule.options).unwrap(), rule.id],
    )?;
    Ok(())
}

pub fn delete_rule(id: i64) -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute("DELETE FROM rules WHERE id=?1", params![id])?;
    Ok(())
}

pub fn delete_all_rules() -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute("DELETE FROM rules", [])?;
    Ok(())
}

pub fn get_watched_folders() -> SqliteResult<Vec<WatchedFolder>> {
    let db = get_db();
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare("SELECT id, path, enabled, mode, only_new FROM watched_folders")?;
    let folders = stmt
        .query_map([], |row| {
            Ok(WatchedFolder {
                id: row.get(0)?,
                path: row.get(1)?,
                enabled: row.get::<_, i32>(2)? != 0,
                mode: row.get(3)?,
                only_new: row.get(4)?,
            })
        })?
        .collect::<SqliteResult<Vec<_>>>()?;
    Ok(folders)
}

pub fn add_watched_folder(path: &str, mode: &str) -> SqliteResult<i64> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT INTO watched_folders (path, enabled, mode) VALUES (?1, 1, ?2)",
        params![path, mode],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn remove_watched_folder(id: i64) -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute("DELETE FROM watched_folders WHERE id=?1", params![id])?;
    conn.execute(
        "DELETE FROM folder_baseline WHERE folder_id=?1",
        params![id],
    )?;
    Ok(())
}

pub fn update_folder_mode(id: i64, mode: &str) -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute(
        "UPDATE watched_folders SET mode=?1 WHERE id=?2",
        params![mode, id],
    )?;
    Ok(())
}

pub fn log_action(log: &ActionLog) -> SqliteResult<i64> {
    let db = get_db();
    let mut conn = db.lock().unwrap();
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO action_logs (timestamp, source_path, destination_path, action, file_name, file_type, undone, run_id, trigger, file_extension, file_size, fingerprint) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9, ?10, ?11)",
        params![
            log.timestamp.to_rfc3339(),
            log.source_path,
            log.destination_path,
            log.action,
            log.file_name,
            log.file_type, log.run_id, log.trigger, log.file_extension, log.file_size, log.fingerprint
        ],
    )?;
    let id = tx.last_insert_rowid();
    if let (Some(path), Some(hash)) = (&log.destination_path, &log.fingerprint) {
        tx.execute(
            "INSERT OR REPLACE INTO processed_files(path,fingerprint,restored) VALUES (?1,?2,0)",
            params![path, hash],
        )?;
    }
    tx.execute(
        "DELETE FROM processed_files WHERE path=?1",
        [&log.source_path],
    )?;
    tx.commit()?;
    Ok(id)
}

pub fn get_recent_logs(limit: i64) -> SqliteResult<Vec<ActionLog>> {
    let db = get_db();
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, source_path, destination_path, action, file_name, file_type, undone, run_id, trigger, file_extension, file_size, fingerprint FROM action_logs ORDER BY id DESC LIMIT ?1"
    )?;
    let logs = stmt
        .query_map(params![limit], |row| {
            let ts_str: String = row.get(1)?;
            Ok(ActionLog {
                id: row.get(0)?,
                timestamp: DateTime::parse_from_rfc3339(&ts_str)
                    .unwrap()
                    .with_timezone(&Utc),
                source_path: row.get(2)?,
                destination_path: row.get(3)?,
                action: row.get(4)?,
                file_name: row.get(5)?,
                file_type: row.get(6)?,
                undone: row.get::<_, i32>(7)? != 0,
                run_id: row.get(8)?,
                trigger: row.get(9)?,
                file_extension: row.get(10)?,
                file_size: row.get(11)?,
                fingerprint: row.get(12)?,
            })
        })?
        .collect::<SqliteResult<Vec<_>>>()?;
    Ok(logs)
}

pub fn get_weekly_stats() -> SqliteResult<Vec<(String, i64)>> {
    let db = get_db();
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT file_type, COUNT(*) FROM action_logs WHERE timestamp > datetime('now', '-7 days') AND undone = 0 GROUP BY file_type"
    )?;
    let stats = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<SqliteResult<Vec<_>>>()?;
    Ok(stats)
}

pub fn get_settings() -> SqliteResult<AppSettings> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.query_row(
        "SELECT id, language, theme, telemetry_enabled, first_run, autostart, grace_period_seconds, lock_check_enabled, auto_update_enabled, schedule_enabled, schedule_times_per_day, schedule_time_1, schedule_time_2, schedule_time_3, schedule_time_4, ai_enabled, ai_model, ai_auto_move, ai_delete_folder FROM settings LIMIT 1",
        [],
        |row| {
            Ok(AppSettings {
                id: row.get(0)?,
                language: row.get(1)?,
                theme: row.get(2)?,
                telemetry_enabled: row.get::<_, i32>(3)? != 0,
                first_run: row.get::<_, i32>(4)? != 0,
                autostart: row.get::<_, i32>(5).unwrap_or(1) != 0,
                grace_period_seconds: row.get::<_, i64>(6).unwrap_or(300),
                lock_check_enabled: row.get::<_, i32>(7).unwrap_or(1) != 0,
                auto_update_enabled: row.get::<_, i32>(8).unwrap_or(1) != 0,
                schedule_enabled: row.get::<_, i32>(9).unwrap_or(0) != 0,
                schedule_times_per_day: row.get::<_, i64>(10).unwrap_or(1),
                schedule_time_1: row.get(11).ok(),
                schedule_time_2: row.get(12).ok(),
                schedule_time_3: row.get(13).ok(),
                schedule_time_4: row.get(14).ok(),
                ai_enabled: row.get::<_, i32>(15).unwrap_or(0) != 0,
                ai_model: row
                    .get::<_, String>(16)
                    .unwrap_or_else(|_| "typesafe/jev-1.14".into()),
                ai_auto_move: row.get::<_, i32>(17).unwrap_or(0) != 0,
                ai_delete_folder: row.get(18).unwrap_or_default(),
            })
        },
    )
}

pub fn update_settings(settings: &AppSettings) -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute(
        "UPDATE settings SET language=?1, theme=?2, telemetry_enabled=?3, first_run=?4, autostart=?5, grace_period_seconds=?6, lock_check_enabled=?7, auto_update_enabled=?8, schedule_enabled=?9, schedule_times_per_day=?10, schedule_time_1=?11, schedule_time_2=?12, schedule_time_3=?13, schedule_time_4=?14, ai_enabled=?15, ai_model=?16, ai_auto_move=?17, ai_delete_folder=?18 WHERE id=?19",
        params![
            settings.language,
            settings.theme,
            settings.telemetry_enabled as i32,
            settings.first_run as i32,
            settings.autostart as i32,
            settings.grace_period_seconds,
            settings.lock_check_enabled as i32,
            settings.auto_update_enabled as i32,
            settings.schedule_enabled as i32,
            settings.schedule_times_per_day,
            settings.schedule_time_1,
            settings.schedule_time_2,
            settings.schedule_time_3,
            settings.schedule_time_4,
            settings.ai_enabled as i32,
            settings.ai_model,
            settings.ai_auto_move as i32,
            settings.ai_delete_folder,
            settings.id
        ],
    )?;
    Ok(())
}

pub fn clear_logs() -> SqliteResult<()> {
    let db = get_db();
    let conn = db.lock().unwrap();
    conn.execute("DELETE FROM action_logs", [])?;
    Ok(())
}
