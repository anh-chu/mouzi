use crate::db::{self, ActionLog, Rule};
use crate::rules::{self, FileInfo};
use chrono::Utc;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
#[cfg(not(target_os = "windows"))]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use std::time::{Duration, Instant};

pub static OPERATION_LOCK: Mutex<()> = Mutex::new(());
static NEXT_ID: AtomicU64 = AtomicU64::new(0);
static PLANS: Lazy<Mutex<HashMap<String, CachedPlan>>> = Lazy::new(|| Mutex::new(HashMap::new()));

pub fn new_run_id() -> String {
    format!(
        "{}-{}-{}",
        Utc::now().timestamp_micros(),
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn fingerprint(path: &Path) -> Result<String, String> {
    if !fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_file()
    {
        return Err("Only regular files are supported".into());
    }
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn suspicious_name(path: &Path) -> bool {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let parts: Vec<_> = name.split('.').collect();
    parts.len() >= 3
        && ["exe", "scr", "com", "bat", "cmd", "ps1", "vbs", "js", "msi"]
            .contains(parts.last().unwrap())
        && parts[..parts.len() - 1].iter().any(|p| {
            [
                "pdf", "doc", "docx", "xls", "xlsx", "jpg", "jpeg", "png", "gif", "zip", "rar",
                "txt",
            ]
            .contains(p)
        })
}

pub fn validate_template(template: &str) -> Result<(), String> {
    let mut sample = template.to_string();
    for token in [
        "{stem}",
        "{filename}",
        "{extension}",
        "{year}",
        "{month}",
        "{day}",
    ] {
        sample = sample.replace(token, "sample");
    }
    if sample.contains(['{', '}']) || !safe_filename(&sample) {
        return Err("validation.template".into());
    }
    Ok(())
}

fn safe_filename(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().to_uppercase();
    !name.is_empty()
        && name.len() <= 240
        && !name.ends_with(['.', ' '])
        && !name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        && ![
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ]
        .contains(&stem.as_str())
}

fn path_key(path: &Path) -> String {
    let text = path.to_string_lossy().to_string();
    if cfg!(target_os = "windows") {
        text.to_lowercase()
    } else {
        text
    }
}

pub fn destination_for(
    file: &FileInfo,
    rule: &Rule,
    reserved: &mut HashSet<String>,
) -> Result<Option<PathBuf>, String> {
    rules::validate_rule(rule)?;
    if ["delete", "ignore"].contains(&rule.action.as_str()) {
        return Ok(None);
    }
    let output = rules::output_file_info(file, rule);
    let parent = file.path.parent().ok_or("Missing parent folder")?;
    let (folder, name) = if rule.action == "rename" {
        let stem = file.path.file_stem().unwrap_or_default().to_string_lossy();
        let extension = file.path.extension().unwrap_or_default().to_string_lossy();
        let now = Utc::now();
        let name = rule
            .options
            .rename_template
            .replace("{stem}", &stem)
            .replace("{filename}", &file.name)
            .replace("{extension}", &extension)
            .replace("{year}", &now.format("%Y").to_string())
            .replace("{month}", &now.format("%m").to_string())
            .replace("{day}", &now.format("%d").to_string());
        let name = if extension.is_empty() {
            name.trim_end_matches('.').to_string()
        } else {
            name
        };
        if !safe_filename(&name) {
            return Err("validation.template".into());
        }
        (parent.to_path_buf(), name)
    } else {
        let folder = rules::resolve_destination(&rule.destination, &output);
        (
            if folder.is_absolute() {
                folder
            } else {
                parent.join(folder)
            },
            output.name,
        )
    };
    let proposed = folder.join(&name);
    if path_key(&proposed) == path_key(&file.path)
        || (proposed.exists()
            && fs::canonicalize(&proposed).ok() == fs::canonicalize(&file.path).ok())
    {
        return Err("preview.unchanged".into());
    }
    let stem = Path::new(&name)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let ext = Path::new(&name)
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    for index in 0..10000 {
        let candidate = if index == 0 {
            proposed.clone()
        } else {
            folder.join(format!("{stem} ({index}){ext}"))
        };
        let key = path_key(&candidate);
        if !candidate.try_exists().map_err(|e| e.to_string())? && !reserved.contains(&key) {
            reserved.insert(key);
            return Ok(Some(candidate));
        }
    }
    Err("Too many destination conflicts".into())
}

// create_new reserves the destination atomically, including on another drive.
// A failing copy leaves the source in place; a failing removal keeps both copies.
pub fn move_without_overwrite(source: &Path, destination: &Path) -> Result<(), String> {
    if !fs::symlink_metadata(source)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_file()
    {
        return Err("Only regular files are supported".into());
    }
    let parent = destination.parent().ok_or("Missing destination folder")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    // Hard links reserve the name without overwriting and preserve NTFS streams.
    match fs::hard_link(source, destination) {
        Ok(()) => {
            return fs::remove_file(source).map_err(|e| {
                format!(
                    "File retained at both {} and {}: {e}",
                    source.display(),
                    destination.display()
                )
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(e.to_string()),
        Err(_) => {}
    }
    #[cfg(target_os = "windows")]
    {
        return copy_between_windows_volumes(source, destination);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut input = fs::File::open(source).map_err(|e| e.to_string())?;
        let metadata = input.metadata().map_err(|e| e.to_string())?;
        let original_hash = fingerprint(source)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .map_err(|e| e.to_string())?;
        let result = (|| -> std::io::Result<()> {
            std::io::copy(&mut input, &mut output)?;
            output.flush()?;
            output.set_times(fs::FileTimes::new().set_modified(metadata.modified()?))?;
            output.set_permissions(metadata.permissions())?;
            output.sync_all()?;
            Ok(())
        })();
        drop(output);
        drop(input);
        if let Err(error) = result {
            let _ = fs::remove_file(destination);
            return Err(error.to_string());
        }
        if fingerprint(source)? != original_hash || fingerprint(destination)? != original_hash {
            return Err(format!(
                "File changed during copy. Both copies were retained: {}",
                destination.display()
            ));
        }
        fs::remove_file(source).map_err(|e| {
            format!(
                "Copy created at {}; source could not be removed: {e}",
                destination.display()
            )
        })
    }
}

#[cfg(target_os = "windows")]
fn copy_between_windows_volumes(source: &Path, destination: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn CopyFileW(existing: *const u16, new: *const u16, fail_if_exists: i32) -> i32;
    }
    let original = fingerprint(source)?;
    let from: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // CopyFileW preserves alternate streams, including Mark of the Web.
    // Both NUL-terminated buffers remain alive for the entire call.
    if unsafe { CopyFileW(from.as_ptr(), to.as_ptr(), 1) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    if fingerprint(source)? != original || fingerprint(destination)? != original {
        return Err(format!(
            "File changed during copy. Both copies were retained: {}",
            destination.display()
        ));
    }
    fs::remove_file(source).map_err(|e| {
        format!(
            "Copy created at {}; source could not be removed: {e}",
            destination.display()
        )
    })
}

pub fn execute_destination(
    source: &Path,
    rule: &Rule,
    destination: Option<&Path>,
) -> Result<(), String> {
    match rule.action.as_str() {
        "move" | "rename" => {
            move_without_overwrite(source, destination.ok_or("Missing destination")?)
        }
        "delete" => trash::delete(source).map_err(|e| e.to_string()),
        "ignore" => Ok(()),
        _ => Err("validation.action".into()),
    }
}

pub fn already_processed(path: &Path) -> Result<bool, String> {
    processed(path, true)
}

fn processed(path: &Path, include_restored: bool) -> Result<bool, String> {
    let db = db::get_db();
    let hashes = db
        .lock()
        .unwrap()
        .prepare("SELECT fingerprint FROM processed_files WHERE path=?1 AND (restored=0 OR ?2=1)")
        .map_err(|e| e.to_string())?
        .query_map(
            rusqlite::params![path.to_string_lossy().as_ref(), include_restored],
            |r| r.get::<_, String>(0),
        )
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(if let Some(hash) = hashes.first() {
        &fingerprint(path)? == hash
    } else {
        false
    })
}

#[derive(Clone, Serialize)]
pub struct PreviewEntry {
    pub id: String,
    pub source: String,
    pub destination: Option<String>,
    pub rule: String,
    pub action: String,
    pub size: u64,
    pub warning: bool,
    pub error: Option<String>,
}
#[derive(Serialize)]
pub struct Preview {
    pub id: String,
    pub entries: Vec<PreviewEntry>,
}
struct Planned {
    entry: PreviewEntry,
    rule: Rule,
    fingerprint: String,
    modified: std::time::SystemTime,
}
struct CachedPlan {
    created: Instant,
    items: Vec<Planned>,
}
#[derive(Serialize)]
pub struct OperationResult {
    pub id: String,
    pub source: String,
    pub success: bool,
    pub error: Option<String>,
}

fn watched_file(path: &Path) -> bool {
    db::get_watched_folders()
        .map(|folders| {
            folders.into_iter().any(|f| {
                f.enabled
                    && f.mode != "paused"
                    && path
                        .parent()
                        .is_some_and(|p| fs::canonicalize(p).ok() == fs::canonicalize(&f.path).ok())
            })
        })
        .unwrap_or(false)
}

pub fn preview(paths: Option<Vec<String>>) -> Result<Preview, String> {
    let _guard = OPERATION_LOCK.lock().unwrap();
    let paths = match paths {
        Some(paths) => paths.into_iter().map(PathBuf::from).collect::<Vec<_>>(),
        None => {
            let mut paths = Vec::new();
            for folder in db::get_watched_folders()
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|f| f.enabled && f.mode != "paused")
            {
                let entries =
                    fs::read_dir(&folder.path).map_err(|e| format!("{}: {e}", folder.path))?;
                paths.extend(
                    entries
                        .filter_map(Result::ok)
                        .map(|e| e.path())
                        .filter(|p| p.is_file()),
                );
            }
            paths
        }
    };
    let id = new_run_id();
    let mut seen = HashSet::new();
    let mut reserved = HashSet::new();
    let mut items = Vec::new();
    let mut entries = Vec::new();
    let mut paths = paths;
    paths.sort();
    for path in paths {
        if !seen.insert(path_key(&path))
            || !watched_file(&path)
            || db::is_baseline_file(&path)
            || rules::should_ignore_file(&path)
            || rules::is_file_ignored_by_mouziignore(&path)
        {
            continue;
        }
        let Some(file) = rules::scan_file(&path) else {
            continue;
        };
        if processed(&path, false)? {
            continue;
        }
        let Some(rule) = rules::find_matching_rule(&file) else {
            continue;
        };
        if rule.action == "ignore" {
            continue;
        }
        let mut entry = PreviewEntry {
            id: new_run_id(),
            source: path.to_string_lossy().into(),
            destination: None,
            rule: rule.name.clone(),
            action: rule.action.clone(),
            size: file.size,
            warning: suspicious_name(&path),
            error: None,
        };
        let result = (|| -> Result<(String, std::time::SystemTime), String> {
            if rules::is_file_locked(&path) {
                return Err("preview.locked".into());
            }
            let dest = destination_for(&file, &rule, &mut reserved)?;
            entry.destination = dest.map(|d| d.to_string_lossy().into());
            let modified = fs::metadata(&path)
                .and_then(|m| m.modified())
                .map_err(|e| e.to_string())?;
            Ok((fingerprint(&path)?, modified))
        })();
        match result {
            Ok((hash, modified)) => items.push(Planned {
                entry: entry.clone(),
                rule,
                fingerprint: hash,
                modified,
            }),
            Err(error) => entry.error = Some(error),
        }
        entries.push(entry);
    }
    let mut plans = PLANS.lock().unwrap();
    plans.retain(|_, plan| plan.created.elapsed() < Duration::from_secs(1800));
    if plans.len() >= 32 {
        return Err("Too many open previews. Close an older preview.".into());
    }
    plans.insert(
        id.clone(),
        CachedPlan {
            created: Instant::now(),
            items,
        },
    );
    Ok(Preview { id, entries })
}

pub fn discard(id: &str) {
    PLANS.lock().unwrap().remove(id);
}

#[cfg(test)]
pub fn apply(id: &str, selected: &[String]) -> Result<Vec<OperationResult>, String> {
    apply_with_trash(id, selected, &[])
}

pub fn apply_with_trash(id: &str, selected: &[String], trash_ids: &[String]) -> Result<Vec<OperationResult>, String> {
    let _guard = OPERATION_LOCK.lock().unwrap();
    let plan = PLANS.lock().unwrap().remove(id).ok_or("preview.expired")?;
    if plan.created.elapsed() > Duration::from_secs(1800) {
        return Err("preview.expired".into());
    }
    let run_id = new_run_id();
    let mut results = Vec::new();
    for item in plan
        .items
        .into_iter()
        .filter(|i| selected.contains(&i.entry.id))
    {
        let result = (|| -> Result<(), String> {
            let path = Path::new(&item.entry.source);
            if !watched_file(path)
                || db::is_baseline_file(path)
                || rules::should_ignore_file(path)
                || rules::is_file_ignored_by_mouziignore(path)
            {
                return Err("preview.changed".into());
            }
            if rules::is_file_locked(path) {
                return Err("preview.locked".into());
            }
            let file = rules::scan_file(path).ok_or("preview.changed")?;
            let rule = rules::find_matching_rule(&file).ok_or("preview.changed")?;
            if serde_json::to_string(&rule).ok() != serde_json::to_string(&item.rule).ok()
                || fingerprint(path)? != item.fingerprint
                || fs::metadata(path)
                    .and_then(|m| m.modified())
                    .map_err(|e| e.to_string())?
                    != item.modified
            {
                return Err("preview.changed".into());
            }
            let ai_trash = trash_ids.contains(&item.entry.id);
            if ai_trash {
                trash::delete(path).map_err(|e| e.to_string())?;
            } else {
                execute_destination(path, &rule, item.entry.destination.as_deref().map(Path::new))?;
            }
            db::log_action(&ActionLog {
                id: None,
                timestamp: Utc::now(),
                source_path: item.entry.source.clone(),
                destination_path: if ai_trash { None } else { item.entry.destination.clone() },
                action: if ai_trash { "delete".into() } else { rule.action },
                file_name: file.name,
                file_type: if ai_trash { "AI triage".into() } else { rule.name },
                undone: false,
                run_id: Some(run_id.clone()),
                trigger: "approved".into(),
                file_extension: file.extension,
                file_size: file.size,
                fingerprint: Some(item.fingerprint.clone()),
            })
            .map_err(|e| format!("Operation completed but history could not be saved: {e}"))?;
            Ok(())
        })();
        results.push(OperationResult {
            id: item.entry.id,
            source: item.entry.source,
            success: result.is_ok(),
            error: result.err(),
        });
    }
    Ok(results)
}

pub fn undo_one(id: i64) -> Result<bool, String> {
    let db = db::get_db();
    let log = db.lock().unwrap().query_row("SELECT source_path,destination_path,fingerprint FROM action_logs WHERE id=?1 AND undone=0 AND action IN ('move','rename')", [id], |r| Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?, r.get::<_,Option<String>>(2)?))).map_err(|e| e.to_string())?;
    let (source, destination, expected) = log;
    let Some(expected) = expected else {
        return Err("history.unverified".into());
    };
    if Path::new(&source).try_exists().map_err(|e| e.to_string())? {
        return Err("history.conflict".into());
    }
    if fingerprint(Path::new(&destination))? != expected {
        return Err("history.changed".into());
    }
    move_without_overwrite(Path::new(&destination), Path::new(&source))?;
    let mut conn = db.lock().unwrap();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute("UPDATE action_logs SET undone=1 WHERE id=?1", [id])
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT OR REPLACE INTO processed_files(path,fingerprint,restored) VALUES (?1,?2,1)",
        rusqlite::params![source, expected],
    )
    .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM processed_files WHERE path=?1", [destination])
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(true)
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct HistoryFilter {
    pub query: String,
    pub rule: String,
    pub extension: String,
    pub from: String,
    pub to: String,
    pub run_id: String,
}
pub fn history(filter: &HistoryFilter) -> Result<Vec<ActionLog>, String> {
    let from = if filter.from.is_empty() {
        None
    } else {
        Some(
            chrono::DateTime::parse_from_rfc3339(&filter.from)
                .map_err(|_| "validation.date")?
                .with_timezone(&Utc),
        )
    };
    let to = if filter.to.is_empty() {
        None
    } else {
        Some(
            chrono::DateTime::parse_from_rfc3339(&filter.to)
                .map_err(|_| "validation.date")?
                .with_timezone(&Utc),
        )
    };
    Ok(db::get_recent_logs(-1)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|log| {
            log.file_name
                .to_lowercase()
                .contains(&filter.query.to_lowercase())
                && log
                    .file_type
                    .to_lowercase()
                    .contains(&filter.rule.to_lowercase())
                && (filter.extension.is_empty()
                    || log
                        .file_extension
                        .eq_ignore_ascii_case(filter.extension.trim_start_matches('.')))
                && from.is_none_or(|date| log.timestamp >= date)
                && to.is_none_or(|date| log.timestamp <= date)
                && (filter.run_id.is_empty()
                    || log.run_id.as_deref() == Some(filter.run_id.as_str()))
        })
        .collect())
}
