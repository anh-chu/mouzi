use crate::db;
use crate::rules;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

const DECISIONS_URL: &str = "https://openrouter.ai/api/alpha/decisions";
const DEFAULT_MODEL: &str = "typesafe/jev-1.14";
const MAX_BATCH_FILES: usize = 64;
const MAX_BATCH_BYTES: usize = 24_000;
const QUESTION_OVERHEAD_BYTES: usize = 320;

const KEEP_CRITERIA: &str = "Worth keeping: personal, financial, legal, medical, or work-related files, photos, documents, source code, and configuration files.";
const TRASH_CRITERIA: &str = "Likely junk: installers, temporary files, partial downloads, duplicates, empty files, or cache dumps.";
const UNSURE_CRITERIA: &str = "Not enough evidence to decide safely.";

#[derive(Debug, Clone, Serialize)]
pub struct TriageFile {
    pub path: String,
    pub name: String,
    pub extension: String,
    pub size: u64,
    pub modified: Option<String>,
    pub rule: Option<String>,
    pub planned_action: Option<String>,
    pub destination: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Classification {
    pub path: String,
    pub verdict: String,
    pub confidence: u8,
    pub probabilities: HashMap<String, f64>,
}

pub fn describe_paths(paths: Vec<String>) -> Vec<TriageFile> {
    paths
        .into_iter()
        .map(|path| describe(PathBuf::from(path)))
        .collect()
}

fn describe(path: PathBuf) -> TriageFile {
    let metadata = fs::metadata(&path).ok();
    let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
    let modified = metadata
        .and_then(|m| m.modified().ok())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|delta| chrono::DateTime::<chrono::Utc>::from_timestamp(delta.as_secs() as i64, 0))
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    let matched = rules::scan_file(&path).and_then(|file| {
        let rule = rules::find_matching_rule(&file)?;
        Some((file, rule))
    });
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    TriageFile {
        path: path.to_string_lossy().into(),
        name,
        extension,
        size,
        modified,
        rule: matched.as_ref().map(|(_, rule)| rule.name.clone()),
        planned_action: matched.as_ref().map(|(_, rule)| rule.action.clone()),
        destination: matched.as_ref().map(|(_, rule)| rule.destination.clone()),
    }
}

pub async fn classify(files: Vec<TriageFile>) -> Result<Vec<Classification>, String> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let settings = db::get_settings().map_err(|e| e.to_string())?;
    if !settings.ai_enabled {
        return Err("ai.disabled".into());
    }
    crate::credentials::migrate_legacy()?;
    let key = crate::credentials::load()?.unwrap_or_default();
    if key.is_empty() {
        return Err("ai.not_configured".into());
    }
    let model = if settings.ai_model.trim().is_empty() {
        DEFAULT_MODEL.to_string()
    } else {
        settings.ai_model.trim().to_string()
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .build()
        .map_err(|e| format!("ai.request_failed: {e}"))?;
    let mut results = Vec::new();
    for batch in build_batches(files) {
        results.extend(request_batch(&client, &key, &model, &batch).await?);
    }
    Ok(results)
}

fn build_batches(files: Vec<TriageFile>) -> Vec<Vec<TriageFile>> {
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut bytes = 0;
    for file in files {
        let size = serde_json::to_string(&file)
            .map(|value| value.len())
            .unwrap_or(0)
            + QUESTION_OVERHEAD_BYTES;
        if !current.is_empty() && (current.len() >= MAX_BATCH_FILES || bytes + size > MAX_BATCH_BYTES)
        {
            batches.push(std::mem::take(&mut current));
            bytes = 0;
        }
        bytes += size;
        current.push(file);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

async fn request_batch(
    client: &reqwest::Client,
    key: &str,
    model: &str,
    files: &[TriageFile],
) -> Result<Vec<Classification>, String> {
    let mut questions = serde_json::Map::new();
    for (index, file) in files.iter().enumerate() {
        let mut criteria = serde_json::Map::new();
        criteria.insert("keep".into(), serde_json::Value::String(KEEP_CRITERIA.into()));
        criteria.insert("trash".into(), serde_json::Value::String(TRASH_CRITERIA.into()));
        criteria.insert("unsure".into(), serde_json::Value::String(UNSURE_CRITERIA.into()));
        let instructions = format!(
            "Consider files[{index}] in the state (name \"{}\", extension \"{}\", {} bytes). Should Mouzi keep this file or move it to the Trash? Choose trash only when the file is very likely junk.",
            file.name, file.extension, file.size
        );
        questions.insert(
            format!("file_{index}"),
            serde_json::json!({
                "type": "choice",
                "instructions": instructions,
                "criteria": criteria,
            }),
        );
    }
    let body = serde_json::json!({
        "model": model,
        "state": { "files": files },
        "questions": questions,
    });
    let response = client
        .post(DECISIONS_URL)
        .header("Authorization", format!("Bearer {key}"))
        .header("HTTP-Referer", "https://github.com/hsr88/mouzi")
        .header("X-Title", "Mouzi")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("ai.request_failed: {e}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| format!("ai.request_failed: {e}"))?;
    if !status.is_success() {
        let message = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(|message| message.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
        return Err(format!("ai.request_failed: {message}"));
    }
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("ai.request_failed: {e}"))?;
    let answers = value
        .get("answers")
        .and_then(|answers| answers.as_object())
        .ok_or_else(|| "ai.request_failed: unexpected response shape".to_string())?;
    let mut results = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let Some(answer) = answers.get(&format!("file_{index}")) else {
            continue;
        };
        let probabilities = answer
            .get("probabilities")
            .and_then(|probabilities| probabilities.as_object())
            .map(|probabilities| {
                probabilities
                    .iter()
                    .filter_map(|(key, value)| value.as_f64().map(|value| (key.clone(), value)))
                    .collect::<HashMap<String, f64>>()
            })
            .unwrap_or_default();
        let verdict = answer
            .get("choice")
            .and_then(|choice| choice.as_str())
            .map(normalize_verdict)
            .unwrap_or_else(|| "unsure".into());
        let confidence = answer
            .get("confidence")
            .and_then(|confidence| confidence.as_f64())
            .unwrap_or(0.0);
        results.push(Classification {
            path: file.path.clone(),
            verdict,
            confidence: (confidence * 100.0).round().clamp(0.0, 100.0) as u8,
            probabilities,
        });
    }
    Ok(results)
}

fn normalize_verdict(raw: &str) -> String {
    let value = raw.trim().to_lowercase();
    if value.contains("trash") || value.contains("delete") || value.contains("discard") {
        "trash".into()
    } else if value.contains("keep") || value.contains("retain") || value.contains("save") {
        "keep".into()
    } else {
        "unsure".into()
    }
}
