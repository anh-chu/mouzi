use crate::db::{get_settings, get_watched_folders, is_folder_manual_mode, is_folder_paused_mode};
use crate::rules::scan_folder_in_run;
use chrono::{Duration, Local, NaiveDate, NaiveTime};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration as StdDuration;
use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;

const CHECK_INTERVAL_SECONDS: u64 = 60;

/// Background scheduler for organizing files at configured local times.
pub struct Scheduler {
    /// Last run date (local) for each schedule slot, to avoid double-triggering.
    last_run_dates: Arc<Mutex<HashMap<usize, NaiveDate>>>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            last_run_dates: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Spawn the scheduler thread. It wakes every 60 seconds and checks whether
    /// any configured schedule time has been reached in the last minute.
    pub fn start(&self, app_handle: AppHandle) {
        let last_run_dates = self.last_run_dates.clone();
        thread::spawn(move || loop {
            thread::sleep(StdDuration::from_secs(CHECK_INTERVAL_SECONDS));
            if let Err(e) = check_and_run(&app_handle, &last_run_dates) {
                eprintln!("[scheduler] error: {}", e);
            }
        });
    }
}

fn check_and_run(
    app_handle: &AppHandle,
    last_run_dates: &Arc<Mutex<HashMap<usize, NaiveDate>>>,
) -> Result<(), String> {
    let settings = get_settings().map_err(|e| e.to_string())?;
    if !settings.schedule_enabled {
        return Ok(());
    }

    let times_per_day = settings.schedule_times_per_day.clamp(1, 4) as usize;
    let schedule_times: Vec<Option<String>> = vec![
        settings.schedule_time_1,
        settings.schedule_time_2,
        settings.schedule_time_3,
        settings.schedule_time_4,
    ];

    let now = Local::now();
    let today = now.date_naive();

    for slot in 0..times_per_day {
        let Some(time_str) = schedule_times.get(slot).and_then(|t| t.as_ref()) else {
            continue;
        };
        let scheduled_time = parse_time(time_str)?;
        let scheduled_dt = today.and_time(scheduled_time);
        let scheduled_local = scheduled_dt
            .and_local_timezone(Local)
            .single()
            .ok_or_else(|| "Invalid local time".to_string())?;
        let diff = now.signed_duration_since(scheduled_local);

        if diff >= Duration::zero() && diff < Duration::minutes(1) {
            let already_run = {
                let guard = last_run_dates.lock().unwrap();
                guard.get(&slot).copied() == Some(today)
            };
            if !already_run {
                let result = perform_scheduled_clean(app_handle);
                last_run_dates.lock().unwrap().insert(slot, today);
                if let Err(error) = result {
                    let _ = app_handle.notification().builder().title("Mouzi: scheduled organization failed").body(&error).show();
                    return Err(error);
                }
            }
        }
    }

    Ok(())
}

fn parse_time(time_str: &str) -> Result<NaiveTime, String> {
    NaiveTime::parse_from_str(time_str.trim(), "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(time_str.trim(), "%H:%M:%S"))
        .map_err(|e| format!("Invalid schedule time '{}': {}", time_str, e))
}

fn perform_scheduled_clean(app_handle: &AppHandle) -> Result<(), String> {
    println!("[scheduler] running scheduled organization");

    let settings = get_settings().map_err(|e| e.to_string())?;
    let folders = get_watched_folders().map_err(|e| e.to_string())?;
    if settings.ai_auto_move {
        if !settings.ai_enabled { return Err("Enable Jev or turn off automatic Jev moving in Settings".into()); }
        let mut paths = Vec::new();
        for folder in folders.iter().filter(|f| f.enabled && crate::db::is_folder_auto_mode(&f.mode)) {
            let entries = std::fs::read_dir(&folder.path).map_err(|e| format!("{}: {e}", folder.path))?;
            paths.extend(entries.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_file()).map(|p| p.to_string_lossy().into_owned()));
        }
        let preview = crate::operations::preview(Some(paths))?;
        let result = tauri::async_runtime::block_on(crate::ai::classify_plan_and_move(&preview.id, "scheduled"));
        let triage = match result { Ok(result) => result, Err(error) => { crate::operations::discard(&preview.id); return Err(error); } };
        let failed = triage.moves.iter().filter(|r| !r.success).count();
        let keeps: Vec<String> = preview.entries.iter().filter(|e| !e.warning && e.error.is_none() && e.action != "delete"
            && triage.classifications.iter().any(|c| c.path == e.source && c.verdict == "keep"))
            .map(|e| e.id.clone()).collect();
        let kept = crate::operations::apply_scheduled(&preview.id, &keeps)?;
        let total = kept.iter().filter(|r| r.success).count() + triage.moves.iter().filter(|r| r.success).count();
        let _ = app_handle.emit("scheduled-clean-done", json!({ "organized": total }));
        let failures = failed + kept.iter().filter(|r| !r.success).count();
        if failures > 0 { return Err(format!("{failures} scheduled file operation(s) failed; check the files and review queue")); }
        return Ok(());
    }
    let mut total = 0usize;
    let run_id = crate::operations::new_run_id();
    for folder in folders {
        if !folder.enabled
            || is_folder_paused_mode(&folder.mode)
            || is_folder_manual_mode(&folder.mode)
        {
            continue;
        }
        if !std::path::Path::new(&folder.path).exists() {
            continue;
        }
        match scan_folder_in_run(&folder.path, &run_id, "scheduled") {
            Ok(results) => total += results.len(),
            Err(e) => eprintln!("[scheduler] failed to clean {}: {}", folder.path, e),
        }
    }

    println!("[scheduler] organized {} files", total);

    let _ = app_handle.emit(
        "scheduled-clean-done",
        json!({
            "organized": total,
        }),
    );

    Ok(())
}
