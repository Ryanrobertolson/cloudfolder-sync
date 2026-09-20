mod providers;

use chrono::{Duration, SecondsFormat, Utc};
use providers::RemoteInfo;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration as StdDuration,
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, State, WindowEvent,
};
use thiserror::Error;

#[derive(Clone)]
struct AppState {
    db_path: PathBuf,
    running_jobs: Arc<Mutex<HashSet<i64>>>,
}

#[derive(Debug, Error)]
enum AppError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("File system error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    Transfer(String),
    #[error("{0}")]
    Cancelled(String),
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

type AppResult<T> = Result<T, AppError>;

const LARGE_FILE_WARNING_BYTES: u64 = 25 * 1024 * 1024;
const SCAN_EXAMPLE_LIMIT: usize = 5;

#[derive(Debug, Clone, Serialize)]
struct SyncJob {
    id: i64,
    name: String,
    source_paths: Vec<String>,
    destination: String,
    interval_minutes: i64,
    backup_mode: String,
    last_full_at: Option<String>,
    enabled: bool,
    last_run_at: Option<String>,
    next_run_at: Option<String>,
    status: String,
    last_message: Option<String>,
    progress_percent: i64,
    progress_message: Option<String>,
    retention_count: i64,
    exclude_patterns: Vec<String>,
    size_filter_mode: String,
    max_file_size_mib: Option<i64>,
    extension_filter_mode: String,
    excluded_extensions: Vec<String>,
    created_at: String,
}

#[derive(Debug, Deserialize)]
struct NewJob {
    name: String,
    source_paths: Vec<String>,
    destination: String,
    interval_minutes: i64,
    backup_mode: String,
    retention_count: i64,
    #[serde(default)]
    exclude_patterns: Vec<String>,
    #[serde(default = "default_filter_mode")]
    size_filter_mode: String,
    #[serde(default)]
    max_file_size_mib: Option<i64>,
    #[serde(default = "default_filter_mode")]
    extension_filter_mode: String,
    #[serde(default)]
    excluded_extensions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AppSettings {
    max_file_size_mib: Option<i64>,
    #[serde(default)]
    excluded_extensions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EffectiveFilters {
    max_file_size_mib: Option<i64>,
    excluded_extensions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct LargeFileExample {
    path: String,
    size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
struct LargeFileScanResult {
    threshold_bytes: u64,
    matching_file_count: u64,
    examples: Vec<LargeFileExample>,
    unreadable_path_count: u64,
    unreadable_examples: Vec<String>,
}

fn default_filter_mode() -> String {
    "inherit".into()
}

#[derive(Debug, Serialize)]
struct RunRecord {
    id: i64,
    job_id: i64,
    started_at: String,
    finished_at: String,
    status: String,
    message: String,
}

#[derive(Debug, Serialize)]
struct ErrorLog {
    id: i64,
    job_id: i64,
    job_name: String,
    started_at: String,
    finished_at: String,
    message: String,
}

#[derive(Debug, Serialize)]
struct ActivityLogEntry {
    id: i64,
    job_id: i64,
    occurred_at: String,
    state: String,
    message: String,
}

#[derive(Debug, Serialize)]
struct CloudFolderEntry {
    name: String,
    path: String,
}

#[derive(Debug, Deserialize)]
struct RcloneListItem {
    #[serde(rename = "Name")]
    name: String,
}

#[derive(Debug, Deserialize)]
struct RcloneVersionItem {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Path", default)]
    path: String,
    #[serde(rename = "Size", default)]
    size: u64,
    #[serde(rename = "ModTime")]
    modified_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct VersionSnapshot {
    name: String,
    created_at: String,
}

#[derive(Debug, Serialize)]
struct VersionFile {
    path: String,
    size: u64,
    modified_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct RestoredVersion {
    path: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    html_url: String,
    published_at: Option<String>,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Debug, Serialize)]
struct UpdateInfo {
    available: bool,
    current_version: String,
    latest_version: String,
    title: String,
    notes: String,
    release_url: String,
    published_at: Option<String>,
    asset_name: Option<String>,
    download_url: Option<String>,
    download_size: Option<u64>,
    package_type: String,
}

#[derive(Debug, Serialize)]
struct DownloadedUpdate {
    path: String,
    instructions: String,
}

fn now_string() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn add_minutes(minutes: i64) -> String {
    (Utc::now() + Duration::minutes(minutes)).to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn connect(path: &Path) -> AppResult<Connection> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(StdDuration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(connection)
}

fn initialize_database(path: &Path) -> AppResult<()> {
    let connection = connect(path)?;
    connection.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS jobs (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            name             TEXT NOT NULL,
            source_path      TEXT NOT NULL,
            destination      TEXT NOT NULL,
            interval_minutes INTEGER NOT NULL,
            backup_mode      TEXT NOT NULL DEFAULT 'incremental',
            last_full_at      TEXT,
            enabled          INTEGER NOT NULL DEFAULT 1,
            last_run_at      TEXT,
            next_run_at      TEXT,
            status           TEXT NOT NULL DEFAULT 'ready',
            last_message     TEXT,
            progress_percent INTEGER NOT NULL DEFAULT 0,
            progress_message TEXT,
            retention_count  INTEGER NOT NULL DEFAULT 5,
            exclude_patterns TEXT NOT NULL DEFAULT '[]',
            size_filter_mode TEXT NOT NULL DEFAULT 'inherit',
            max_file_size_mib INTEGER,
            extension_filter_mode TEXT NOT NULL DEFAULT 'inherit',
            excluded_extensions TEXT NOT NULL DEFAULT '[]',
            cancel_requested INTEGER NOT NULL DEFAULT 0,
            created_at       TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS app_settings (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            max_file_size_mib INTEGER,
            excluded_extensions TEXT NOT NULL DEFAULT '[]'
        );

        INSERT OR IGNORE INTO app_settings (id, max_file_size_mib, excluded_extensions)
        VALUES (1, NULL, '[]');

        CREATE TABLE IF NOT EXISTS runs (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            job_id      INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
            started_at  TEXT NOT NULL,
            finished_at TEXT NOT NULL,
            status      TEXT NOT NULL,
            message     TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS backup_activity (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            job_id      INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
            occurred_at TEXT NOT NULL,
            state       TEXT NOT NULL,
            message     TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS file_cache (
            job_id       INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
            source_index INTEGER NOT NULL,
            relative_path TEXT NOT NULL,
            mtime_ns     INTEGER NOT NULL,
            size_bytes   INTEGER NOT NULL,
            PRIMARY KEY (job_id, source_index, relative_path)
        );

        CREATE INDEX IF NOT EXISTS idx_jobs_due
            ON jobs(enabled, next_run_at);
        CREATE INDEX IF NOT EXISTS idx_runs_job
            ON runs(job_id, id DESC);
        CREATE INDEX IF NOT EXISTS idx_backup_activity_job
            ON backup_activity(job_id, id DESC);
        CREATE INDEX IF NOT EXISTS idx_file_cache_job
            ON file_cache(job_id, source_index);
        ",
    )?;
    ensure_job_column(
        &connection,
        "backup_mode",
        "ALTER TABLE jobs ADD COLUMN backup_mode TEXT NOT NULL DEFAULT 'incremental'",
    )?;
    ensure_job_column(
        &connection,
        "last_full_at",
        "ALTER TABLE jobs ADD COLUMN last_full_at TEXT",
    )?;
    ensure_job_column(
        &connection,
        "progress_percent",
        "ALTER TABLE jobs ADD COLUMN progress_percent INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_job_column(
        &connection,
        "progress_message",
        "ALTER TABLE jobs ADD COLUMN progress_message TEXT",
    )?;
    ensure_job_column(
        &connection,
        "retention_count",
        "ALTER TABLE jobs ADD COLUMN retention_count INTEGER NOT NULL DEFAULT 5",
    )?;
    ensure_job_column(
        &connection,
        "exclude_patterns",
        "ALTER TABLE jobs ADD COLUMN exclude_patterns TEXT NOT NULL DEFAULT '[]'",
    )?;
    ensure_job_column(
        &connection,
        "cancel_requested",
        "ALTER TABLE jobs ADD COLUMN cancel_requested INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_job_column(
        &connection,
        "size_filter_mode",
        "ALTER TABLE jobs ADD COLUMN size_filter_mode TEXT NOT NULL DEFAULT 'inherit'",
    )?;
    ensure_job_column(
        &connection,
        "max_file_size_mib",
        "ALTER TABLE jobs ADD COLUMN max_file_size_mib INTEGER",
    )?;
    ensure_job_column(
        &connection,
        "extension_filter_mode",
        "ALTER TABLE jobs ADD COLUMN extension_filter_mode TEXT NOT NULL DEFAULT 'inherit'",
    )?;
    ensure_job_column(
        &connection,
        "excluded_extensions",
        "ALTER TABLE jobs ADD COLUMN excluded_extensions TEXT NOT NULL DEFAULT '[]'",
    )?;
    Ok(())
}

fn reset_interrupted_jobs(path: &Path) -> AppResult<()> {
    let connection = connect(path)?;
    connection.execute(
        "INSERT INTO backup_activity (job_id, occurred_at, state, message)
         SELECT id, ?1, 'error',
                'The previous backup stopped before it could finish.'
         FROM jobs WHERE status = 'running'",
        [now_string()],
    )?;
    connection.execute(
        "UPDATE jobs
         SET status = 'ready', last_message = 'Previous run was interrupted',
             progress_percent = 0, progress_message = NULL, cancel_requested = 0
         WHERE status = 'running'",
        [],
    )?;
    Ok(())
}

fn record_activity(
    connection: &Connection,
    job_id: i64,
    state: &str,
    message: &str,
) -> AppResult<()> {
    connection.execute(
        "INSERT INTO backup_activity (job_id, occurred_at, state, message)
         VALUES (?1, ?2, ?3, ?4)",
        params![job_id, now_string(), state, message],
    )?;
    connection.execute(
        "DELETE FROM backup_activity
         WHERE job_id = ?1 AND id NOT IN (
             SELECT id FROM backup_activity
             WHERE job_id = ?1 ORDER BY id DESC LIMIT 500
         )",
        [job_id],
    )?;
    Ok(())
}

fn ensure_job_column(connection: &Connection, column: &str, migration: &str) -> AppResult<()> {
    let mut statement = connection.prepare("PRAGMA table_info(jobs)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !columns.iter().any(|existing| existing == column) {
        connection.execute(migration, [])?;
    }
    Ok(())
}

fn map_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<SyncJob> {
    let stored_source: String = row.get(2)?;
    let stored_exclude_patterns: String = row.get(15)?;
    let stored_excluded_extensions: String = row.get(19)?;
    Ok(SyncJob {
        id: row.get(0)?,
        name: row.get(1)?,
        source_paths: decode_source_paths(&stored_source),
        destination: row.get(3)?,
        interval_minutes: row.get(4)?,
        backup_mode: row.get(5)?,
        last_full_at: row.get(6)?,
        enabled: row.get::<_, i64>(7)? != 0,
        last_run_at: row.get(8)?,
        next_run_at: row.get(9)?,
        status: row.get(10)?,
        last_message: row.get(11)?,
        progress_percent: row.get(12)?,
        progress_message: row.get(13)?,
        retention_count: row.get(14)?,
        exclude_patterns: decode_exclude_patterns(&stored_exclude_patterns),
        size_filter_mode: row.get(16)?,
        max_file_size_mib: row.get(17)?,
        extension_filter_mode: row.get(18)?,
        excluded_extensions: decode_exclude_patterns(&stored_excluded_extensions),
        created_at: row.get(20)?,
    })
}

fn decode_source_paths(stored: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(stored)
        .ok()
        .filter(|paths| !paths.is_empty())
        .unwrap_or_else(|| vec![stored.to_owned()])
}

fn decode_exclude_patterns(stored: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(stored).unwrap_or_default()
}

fn clean_exclude_patterns(patterns: &[String]) -> Vec<String> {
    patterns
        .iter()
        .map(|pattern| pattern.trim().to_owned())
        .filter(|pattern| !pattern.is_empty())
        .collect()
}

fn clean_extensions(extensions: &[String]) -> AppResult<Vec<String>> {
    if extensions.len() > 100 {
        return Err(AppError::Validation(
            "You can skip up to 100 file extensions".into(),
        ));
    }
    let mut cleaned = Vec::with_capacity(extensions.len());
    let mut seen = HashSet::new();
    for extension in extensions {
        let trimmed = extension.trim();
        let normalized = trimmed
            .strip_prefix('.')
            .unwrap_or(trimmed)
            .to_ascii_lowercase();
        if normalized.is_empty()
            || normalized.len() > 32
            || !normalized.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '_')
            })
        {
            return Err(AppError::Validation(
                "File extensions must be 1–32 letters or numbers; +, - and _ are also allowed"
                    .into(),
            ));
        }
        if !seen.insert(normalized.clone()) {
            return Err(AppError::Validation(
                "The same file extension was added more than once".into(),
            ));
        }
        cleaned.push(normalized);
    }
    Ok(cleaned)
}

fn validate_max_file_size(value: Option<i64>) -> AppResult<()> {
    if value.is_some_and(|size| !(1..=8_388_608).contains(&size)) {
        return Err(AppError::Validation(
            "Maximum file size must be between 1 MiB and 8,388,608 MiB".into(),
        ));
    }
    Ok(())
}

fn validate_filter_mode(mode: &str) -> AppResult<()> {
    if matches!(mode, "inherit" | "custom" | "disabled") {
        Ok(())
    } else {
        Err(AppError::Validation(
            "Choose whether this backup inherits, customizes, or disables the app filter".into(),
        ))
    }
}

fn resolve_filter_values(
    size_mode: &str,
    max_file_size_mib: Option<i64>,
    extension_mode: &str,
    excluded_extensions: &[String],
    settings: &AppSettings,
) -> AppResult<EffectiveFilters> {
    validate_filter_mode(size_mode)?;
    validate_filter_mode(extension_mode)?;
    let max_file_size_mib = match size_mode {
        "inherit" => settings.max_file_size_mib,
        "custom" => max_file_size_mib,
        "disabled" => None,
        _ => unreachable!(),
    };
    if size_mode == "custom" && max_file_size_mib.is_none() {
        return Err(AppError::Validation(
            "Enter a maximum file size for this backup".into(),
        ));
    }
    validate_max_file_size(max_file_size_mib)?;
    let excluded_extensions = match extension_mode {
        "inherit" => settings.excluded_extensions.clone(),
        "custom" => clean_extensions(excluded_extensions)?,
        "disabled" => Vec::new(),
        _ => unreachable!(),
    };
    Ok(EffectiveFilters {
        max_file_size_mib,
        excluded_extensions,
    })
}

fn load_app_settings(connection: &Connection) -> AppResult<AppSettings> {
    let (max_file_size_mib, stored_extensions): (Option<i64>, String) = connection.query_row(
        "SELECT max_file_size_mib, excluded_extensions FROM app_settings WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(AppSettings {
        max_file_size_mib,
        excluded_extensions: decode_exclude_patterns(&stored_extensions),
    })
}

fn scan_paths_for_large_files(
    paths: &[String],
    threshold_bytes: u64,
) -> AppResult<LargeFileScanResult> {
    if paths.is_empty() {
        return Err(AppError::Validation(
            "Choose at least one file or folder to scan".into(),
        ));
    }
    let mut result = LargeFileScanResult {
        threshold_bytes,
        matching_file_count: 0,
        examples: Vec::new(),
        unreadable_path_count: 0,
        unreadable_examples: Vec::new(),
    };
    let mut inspected_root = false;
    let mut visited = HashSet::new();
    let mut pending = paths.iter().map(PathBuf::from).collect::<Vec<_>>();
    while let Some(path) = pending.pop() {
        if !visited.insert(path.clone()) {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                result.unreadable_path_count += 1;
                if result.unreadable_examples.len() < SCAN_EXAMPLE_LIMIT {
                    result
                        .unreadable_examples
                        .push(path.to_string_lossy().into_owned());
                }
                continue;
            }
        };
        if paths.iter().any(|root| Path::new(root) == path) {
            inspected_root = true;
        }
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            match std::fs::read_dir(&path) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(entry) => pending.push(entry.path()),
                            Err(_) => {
                                result.unreadable_path_count += 1;
                                if result.unreadable_examples.len() < SCAN_EXAMPLE_LIMIT {
                                    result
                                        .unreadable_examples
                                        .push(path.to_string_lossy().into_owned());
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    result.unreadable_path_count += 1;
                    if result.unreadable_examples.len() < SCAN_EXAMPLE_LIMIT {
                        result
                            .unreadable_examples
                            .push(path.to_string_lossy().into_owned());
                    }
                }
            }
            continue;
        }
        if file_type.is_file() && metadata.len() > threshold_bytes {
            result.matching_file_count += 1;
            result.examples.push(LargeFileExample {
                path: path.to_string_lossy().into_owned(),
                size_bytes: metadata.len(),
            });
            result
                .examples
                .sort_by_key(|example| std::cmp::Reverse(example.size_bytes));
            result.examples.truncate(SCAN_EXAMPLE_LIMIT);
        }
    }
    if !inspected_root {
        return Err(AppError::Validation(
            "None of the selected files or folders could be inspected".into(),
        ));
    }
    Ok(result)
}

#[tauri::command]
async fn scan_large_files(paths: Vec<String>) -> AppResult<LargeFileScanResult> {
    tokio::task::spawn_blocking(move || {
        scan_paths_for_large_files(&paths, LARGE_FILE_WARNING_BYTES)
    })
    .await
    .map_err(|error| AppError::Transfer(format!("The large-file scan stopped: {error}")))?
}

fn get_job(connection: &Connection, job_id: i64) -> AppResult<SyncJob> {
    connection
        .query_row(
            "SELECT id, name, source_path, destination, interval_minutes,
                    backup_mode, last_full_at, enabled, last_run_at, next_run_at,
                    status, last_message, progress_percent, progress_message,
                    retention_count, exclude_patterns, size_filter_mode,
                    max_file_size_mib, extension_filter_mode, excluded_extensions,
                    created_at
             FROM jobs WHERE id = ?1",
            [job_id],
            map_job,
        )
        .optional()?
        .ok_or_else(|| AppError::Validation(format!("Backup job {job_id} was not found")))
}

fn validate_new_job(input: &NewJob) -> AppResult<()> {
    if input.name.trim().is_empty() {
        return Err(AppError::Validation("Give this backup a name".into()));
    }
    if input.interval_minutes < 1 {
        return Err(AppError::Validation(
            "The schedule must be at least one minute".into(),
        ));
    }
    if !matches!(
        input.backup_mode.as_str(),
        "full" | "incremental" | "differential" | "mirror"
    ) {
        return Err(AppError::Validation(
            "Choose a supported backup mode".into(),
        ));
    }
    if !(0..=50).contains(&input.retention_count) {
        return Err(AppError::Validation(
            "Choose between 1 and 50 previous backups, or turn previous files off".into(),
        ));
    }
    validate_filter_mode(&input.size_filter_mode)?;
    validate_filter_mode(&input.extension_filter_mode)?;
    if input.size_filter_mode == "custom" && input.max_file_size_mib.is_none() {
        return Err(AppError::Validation(
            "Enter a maximum file size for this backup".into(),
        ));
    }
    validate_max_file_size(input.max_file_size_mib)?;
    clean_extensions(&input.excluded_extensions)?;
    if input.exclude_patterns.len() > 100 {
        return Err(AppError::Validation(
            "A backup can have up to 100 ignore rules".into(),
        ));
    }
    let cleaned_patterns = clean_exclude_patterns(&input.exclude_patterns);
    if cleaned_patterns.len() != input.exclude_patterns.len() {
        return Err(AppError::Validation("Remove any empty ignore rules".into()));
    }
    let mut seen_patterns = HashSet::new();
    for pattern in &cleaned_patterns {
        if pattern.len() > 512 || pattern.contains(['\n', '\r', '\0']) {
            return Err(AppError::Validation(
                "Each ignore rule must be one line and no more than 512 characters".into(),
            ));
        }
        if matches!(pattern.as_str(), "*" | "**" | "/**" | "/**/*") {
            return Err(AppError::Validation(
                "That ignore rule would skip the entire backup".into(),
            ));
        }
        if !seen_patterns.insert(pattern.to_lowercase()) {
            return Err(AppError::Validation(
                "The same ignore rule was added more than once".into(),
            ));
        }
    }
    if input.source_paths.is_empty() {
        return Err(AppError::Validation(
            "Choose at least one file or folder to back up".into(),
        ));
    }
    if input.source_paths.len() > 50 {
        return Err(AppError::Validation(
            "A backup can contain up to 50 files and folders".into(),
        ));
    }

    let mut seen_paths = HashSet::new();
    let mut seen_names = HashSet::new();
    for source_path in &input.source_paths {
        let source = Path::new(source_path);
        if !source.is_absolute() {
            return Err(AppError::Validation(format!(
                "The selected source must be an absolute path: {source_path}"
            )));
        }
        if !source.exists() {
            return Err(AppError::Validation(format!(
                "The selected file or folder no longer exists: {source_path}"
            )));
        }
        if input.backup_mode == "mirror" && source.is_file() {
            return Err(AppError::Validation(
                "Mirroring works with folders only. Put the file in a folder or choose another backup type."
                    .into(),
            ));
        }
        if !seen_paths.insert(source_path) {
            return Err(AppError::Validation(
                "The same source was selected more than once".into(),
            ));
        }
        if input.source_paths.len() > 1 {
            let name = source.file_name().ok_or_else(|| {
                AppError::Validation(
                    "The file-system root cannot be combined with other sources".into(),
                )
            })?;
            let folded_name = name.to_string_lossy().to_lowercase();
            if !seen_names.insert(folded_name) {
                return Err(AppError::Validation(
                    "Two selected sources have the same name. Rename one or put it in a separate backup."
                        .into(),
                ));
            }
        }
    }
    if !input.destination.contains(':') {
        return Err(AppError::Validation(
            "Choose a configured cloud connection".into(),
        ));
    }
    Ok(())
}

#[tauri::command]
fn list_jobs(state: State<'_, AppState>) -> AppResult<Vec<SyncJob>> {
    let connection = connect(&state.db_path)?;
    let mut statement = connection.prepare(
        "SELECT id, name, source_path, destination, interval_minutes,
                backup_mode, last_full_at, enabled, last_run_at, next_run_at,
                status, last_message, progress_percent, progress_message,
                retention_count, exclude_patterns, size_filter_mode,
                max_file_size_mib, extension_filter_mode, excluded_extensions,
                created_at
         FROM jobs ORDER BY created_at DESC",
    )?;
    let jobs = statement
        .query_map([], map_job)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(jobs)
}

#[tauri::command]
fn get_app_settings(state: State<'_, AppState>) -> AppResult<AppSettings> {
    let connection = connect(&state.db_path)?;
    load_app_settings(&connection)
}

#[tauri::command]
fn update_app_settings(input: AppSettings, state: State<'_, AppState>) -> AppResult<AppSettings> {
    let mut connection = connect(&state.db_path)?;
    update_app_settings_record(&mut connection, input)
}

fn update_app_settings_record(
    connection: &mut Connection,
    input: AppSettings,
) -> AppResult<AppSettings> {
    validate_max_file_size(input.max_file_size_mib)?;
    let cleaned_extensions = clean_extensions(&input.excluded_extensions)?;
    let stored_extensions = serde_json::to_string(&cleaned_extensions).map_err(|error| {
        AppError::Validation(format!("Could not save the file extensions: {error}"))
    })?;
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let backup_running = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM jobs WHERE status = 'running')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if backup_running {
        return Err(AppError::Validation(
            "Wait for running backups to finish before changing app-wide filters".into(),
        ));
    }
    let current = load_app_settings(&transaction)?;
    let size_changed = current.max_file_size_mib != input.max_file_size_mib;
    let extensions_changed = current.excluded_extensions != cleaned_extensions;
    transaction.execute(
        "UPDATE app_settings
         SET max_file_size_mib = ?1, excluded_extensions = ?2
         WHERE id = 1",
        params![input.max_file_size_mib, stored_extensions],
    )?;
    if size_changed {
        transaction.execute(
            "UPDATE jobs SET last_full_at = NULL WHERE size_filter_mode = 'inherit'",
            [],
        )?;
        transaction.execute(
            "DELETE FROM file_cache WHERE job_id IN (SELECT id FROM jobs WHERE size_filter_mode = 'inherit')",
            [],
        )?;
    }
    if extensions_changed {
        transaction.execute(
            "UPDATE jobs SET last_full_at = NULL WHERE extension_filter_mode = 'inherit'",
            [],
        )?;
        transaction.execute(
            "DELETE FROM file_cache WHERE job_id IN (SELECT id FROM jobs WHERE extension_filter_mode = 'inherit')",
            [],
        )?;
    }
    transaction.commit()?;
    load_app_settings(connection)
}

#[tauri::command]
fn create_job(input: NewJob, state: State<'_, AppState>) -> AppResult<SyncJob> {
    validate_new_job(&input)?;
    let connection = connect(&state.db_path)?;
    let created_at = now_string();
    let next_run = add_minutes(input.interval_minutes);
    let stored_sources = serde_json::to_string(&input.source_paths)
        .map_err(|error| AppError::Validation(format!("Could not save the sources: {error}")))?;
    let stored_exclude_patterns = serde_json::to_string(&clean_exclude_patterns(
        &input.exclude_patterns,
    ))
    .map_err(|error| AppError::Validation(format!("Could not save the ignore rules: {error}")))?;
    let stored_excluded_extensions =
        serde_json::to_string(&clean_extensions(&input.excluded_extensions)?).map_err(|error| {
            AppError::Validation(format!("Could not save the file extensions: {error}"))
        })?;
    connection.execute(
        "INSERT INTO jobs
            (name, source_path, destination, interval_minutes, backup_mode,
             retention_count, exclude_patterns, size_filter_mode, max_file_size_mib,
             extension_filter_mode, excluded_extensions, enabled, next_run_at, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 1, ?12, 'ready', ?13)",
        params![
            input.name.trim(),
            stored_sources,
            input.destination,
            input.interval_minutes,
            input.backup_mode,
            input.retention_count,
            stored_exclude_patterns,
            input.size_filter_mode,
            input.max_file_size_mib,
            input.extension_filter_mode,
            stored_excluded_extensions,
            next_run,
            created_at
        ],
    )?;
    get_job(&connection, connection.last_insert_rowid())
}

fn update_job_record(connection: &Connection, job_id: i64, input: &NewJob) -> AppResult<SyncJob> {
    let current = get_job(connection, job_id)?;
    let stored_sources = serde_json::to_string(&input.source_paths)
        .map_err(|error| AppError::Validation(format!("Could not save the sources: {error}")))?;
    let cleaned_exclude_patterns = clean_exclude_patterns(&input.exclude_patterns);
    let stored_exclude_patterns =
        serde_json::to_string(&cleaned_exclude_patterns).map_err(|error| {
            AppError::Validation(format!("Could not save the ignore rules: {error}"))
        })?;
    let cleaned_extensions = clean_extensions(&input.excluded_extensions)?;
    let stored_excluded_extensions =
        serde_json::to_string(&cleaned_extensions).map_err(|error| {
            AppError::Validation(format!("Could not save the file extensions: {error}"))
        })?;
    let next_run = if current.enabled {
        Some(add_minutes(input.interval_minutes))
    } else {
        None
    };
    let baseline_still_valid = current.source_paths == input.source_paths
        && current.destination == input.destination
        && current.backup_mode == input.backup_mode
        && current.exclude_patterns == cleaned_exclude_patterns
        && current.size_filter_mode == input.size_filter_mode
        && current.max_file_size_mib == input.max_file_size_mib
        && current.extension_filter_mode == input.extension_filter_mode
        && current.excluded_extensions == cleaned_extensions;
    let last_full_at = baseline_still_valid
        .then_some(current.last_full_at)
        .flatten();
    connection.execute(
        "UPDATE jobs
         SET name = ?1, source_path = ?2, destination = ?3,
             interval_minutes = ?4, backup_mode = ?5, last_full_at = ?6,
             retention_count = ?7, exclude_patterns = ?8, size_filter_mode = ?9,
             max_file_size_mib = ?10, extension_filter_mode = ?11,
             excluded_extensions = ?12, next_run_at = ?13, status = ?14,
             progress_percent = 0, progress_message = NULL
         WHERE id = ?15",
        params![
            input.name.trim(),
            stored_sources,
            input.destination,
            input.interval_minutes,
            input.backup_mode,
            last_full_at,
            input.retention_count,
            stored_exclude_patterns,
            input.size_filter_mode,
            input.max_file_size_mib,
            input.extension_filter_mode,
            stored_excluded_extensions,
            next_run,
            if current.enabled { "ready" } else { "paused" },
            job_id
        ],
    )?;
    if !baseline_still_valid {
        connection.execute("DELETE FROM file_cache WHERE job_id = ?1", [job_id])?;
    }
    get_job(connection, job_id)
}

#[tauri::command]
fn update_job(job_id: i64, input: NewJob, state: State<'_, AppState>) -> AppResult<SyncJob> {
    validate_new_job(&input)?;
    if state
        .running_jobs
        .lock()
        .map_err(|_| AppError::Transfer("The scheduler lock is unavailable".into()))?
        .contains(&job_id)
    {
        return Err(AppError::Validation(
            "Wait for this backup to finish before editing it".into(),
        ));
    }

    let connection = connect(&state.db_path)?;
    update_job_record(&connection, job_id, &input)
}

#[tauri::command]
fn set_job_enabled(job_id: i64, enabled: bool, state: State<'_, AppState>) -> AppResult<SyncJob> {
    let connection = connect(&state.db_path)?;
    let job = get_job(&connection, job_id)?;
    if job.status == "running" {
        return Err(AppError::Validation(
            "Cancel the active backup before changing its schedule".into(),
        ));
    }
    let next_run = if enabled {
        Some(add_minutes(job.interval_minutes))
    } else {
        None
    };
    connection.execute(
        "UPDATE jobs
         SET enabled = ?1, next_run_at = ?2, status = ?3,
             progress_percent = 0, progress_message = NULL
         WHERE id = ?4",
        params![
            i64::from(enabled),
            next_run,
            if enabled { "ready" } else { "paused" },
            job_id
        ],
    )?;
    get_job(&connection, job_id)
}

fn request_job_cancellation(connection: &Connection, job_id: i64) -> AppResult<String> {
    get_job(connection, job_id)?;
    let message = "Cancel requested. Stopping the active cloud transfer safely…";
    let changed = connection.execute(
        "UPDATE jobs
         SET cancel_requested = 1, progress_message = 'Stopping safely…'
         WHERE id = ?1 AND status = 'running'",
        [job_id],
    )?;
    if changed == 0 {
        return Err(AppError::Validation(
            "This backup is not currently running".into(),
        ));
    }
    record_activity(connection, job_id, "waiting", message)?;
    Ok(message.into())
}

#[tauri::command]
fn cancel_job(job_id: i64, state: State<'_, AppState>) -> AppResult<String> {
    let connection = connect(&state.db_path)?;
    request_job_cancellation(&connection, job_id)
}

#[tauri::command]
fn delete_job(job_id: i64, state: State<'_, AppState>) -> AppResult<()> {
    if state
        .running_jobs
        .lock()
        .map_err(|_| AppError::Transfer("The scheduler lock is unavailable".into()))?
        .contains(&job_id)
    {
        return Err(AppError::Validation(
            "Wait for this backup to finish before removing it".into(),
        ));
    }
    let connection = connect(&state.db_path)?;
    let changed = connection.execute("DELETE FROM jobs WHERE id = ?1", [job_id])?;
    if changed == 0 {
        return Err(AppError::Validation("Backup job was not found".into()));
    }
    Ok(())
}

fn compact_output(stdout: &[u8], stderr: &[u8]) -> String {
    let mut combined = String::new();
    let out = String::from_utf8_lossy(stdout);
    let err = String::from_utf8_lossy(stderr);
    if !out.trim().is_empty() {
        combined.push_str(out.trim());
    }
    if !err.trim().is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(err.trim());
    }
    if combined.is_empty() {
        return "Backup completed; no files needed uploading".into();
    }
    let mut chars: Vec<char> = combined.chars().collect();
    if chars.len() > 4_000 {
        chars = chars.split_off(chars.len() - 4_000);
        format!("…{}", chars.into_iter().collect::<String>())
    } else {
        chars.into_iter().collect()
    }
}

#[derive(Debug)]
struct CopyOutcome {
    message: String,
    full_completed: bool,
}

fn backup_stamp() -> String {
    Utc::now().format("%Y-%m-%d_%H-%M-%S").to_string()
}

fn supports_previous_files(job: &SyncJob) -> bool {
    job.retention_count > 0 && matches!(job.backup_mode.as_str(), "incremental" | "mirror")
}

fn version_history_root(job: &SyncJob) -> AppResult<String> {
    let remote_end = if job.destination.starts_with(':') {
        job.destination[1..].find(':').map(|index| index + 2)
    } else {
        job.destination.find(':').map(|index| index + 1)
    }
    .ok_or_else(|| AppError::Validation("The backup destination has no cloud connection".into()))?;
    let remote = &job.destination[..remote_end];
    let history_parent = if remote == ":local:" {
        let local_destination = Path::new(&job.destination[remote_end..]);
        let parent = local_destination.parent().unwrap_or_else(|| Path::new("/"));
        format!("{remote}{}", parent.display())
    } else {
        remote.to_owned()
    };
    Ok(child_cloud_destination(
        &child_cloud_destination(&history_parent, "CloudFolder Previous Files"),
        &format!("Backup {}", job.id),
    ))
}

fn version_snapshot_destination(job: &SyncJob, snapshot_name: &str) -> AppResult<String> {
    Ok(child_cloud_destination(
        &version_history_root(job)?,
        snapshot_name,
    ))
}

fn snapshot_created_at(name: &str) -> Option<String> {
    chrono::NaiveDateTime::parse_from_str(name, "%Y-%m-%d_%H-%M-%S")
        .ok()
        .map(|value| value.and_utc().to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn validate_snapshot_name(name: &str) -> AppResult<()> {
    if snapshot_created_at(name).is_none() {
        return Err(AppError::Validation(
            "That previous backup name is not valid".into(),
        ));
    }
    Ok(())
}

fn validate_version_file_path(path: &str) -> AppResult<()> {
    if path.is_empty()
        || path.chars().any(char::is_control)
        || Path::new(path)
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(AppError::Validation(
            "That previous file path is not valid".into(),
        ));
    }
    Ok(())
}

fn ensure_version_history(job: &SyncJob) -> AppResult<()> {
    let root = version_history_root(job)?;
    let output = Command::new("rclone")
        .arg("mkdir")
        .arg(root)
        .output()
        .map_err(|error| {
            AppError::Transfer(format!(
                "Could not prepare the Previous files folder: {error}"
            ))
        })?;
    if !output.status.success() {
        return Err(AppError::Transfer(format!(
            "CloudFolder could not prepare the Previous files folder. {}",
            compact_output(&output.stdout, &output.stderr)
        )));
    }
    Ok(())
}

fn version_snapshots_for_job(job: &SyncJob) -> AppResult<Vec<VersionSnapshot>> {
    if !supports_previous_files(job) {
        return Ok(Vec::new());
    }
    let output = Command::new("rclone")
        .arg("lsjson")
        .arg(version_history_root(job)?)
        .arg("--dirs-only")
        .arg("--fast-list")
        .arg("--no-modtime")
        .arg("--no-mimetype")
        .output()
        .map_err(|error| AppError::Transfer(format!("Could not read previous backups: {error}")))?;
    if !output.status.success() {
        if output.status.code() == Some(3) {
            return Ok(Vec::new());
        }
        return Err(AppError::Transfer(format!(
            "CloudFolder could not read previous backups. {}",
            compact_output(&output.stdout, &output.stderr)
        )));
    }
    let mut snapshots = serde_json::from_slice::<Vec<RcloneListItem>>(&output.stdout)
        .map_err(|error| {
            AppError::Transfer(format!(
                "The Previous files folder returned an unreadable list: {error}"
            ))
        })?
        .into_iter()
        .filter_map(|item| {
            snapshot_created_at(&item.name).map(|created_at| VersionSnapshot {
                name: item.name,
                created_at,
            })
        })
        .collect::<Vec<_>>();
    snapshots.sort_by(|left, right| right.name.cmp(&left.name));
    Ok(snapshots)
}

fn prune_version_history(job: &SyncJob) -> AppResult<()> {
    let snapshots = version_snapshots_for_job(job)?;
    for snapshot in snapshots
        .into_iter()
        .skip(job.retention_count.max(0) as usize)
    {
        let output = Command::new("rclone")
            .arg("purge")
            .arg(version_snapshot_destination(job, &snapshot.name)?)
            .arg("--drive-use-trash=false")
            .arg("--fast-list")
            .output()
            .map_err(|error| {
                AppError::Transfer(format!(
                    "Could not remove an expired previous backup: {error}"
                ))
            })?;
        if !output.status.success() {
            return Err(AppError::Transfer(format!(
                "CloudFolder could not remove an expired previous backup. {}",
                compact_output(&output.stdout, &output.stderr)
            )));
        }
    }
    Ok(())
}

#[derive(Clone)]
struct ProgressReporter {
    db_path: PathBuf,
    job_id: i64,
}

impl ProgressReporter {
    fn report(&self, percent: i64, message: &str) {
        let Ok(connection) = connect(&self.db_path) else {
            return;
        };
        let _ = connection.execute(
            "UPDATE jobs
             SET progress_percent = ?1, progress_message = ?2
             WHERE id = ?3 AND status = 'running'",
            params![percent.clamp(0, 100), message, self.job_id],
        );
    }

    fn activity(&self, state: &str, message: &str) {
        let Ok(connection) = connect(&self.db_path) else {
            return;
        };
        let _ = record_activity(&connection, self.job_id, state, message);
    }

    fn cancellation_requested(&self) -> bool {
        let Ok(connection) = connect(&self.db_path) else {
            return false;
        };
        connection
            .query_row(
                "SELECT cancel_requested FROM jobs WHERE id = ?1",
                [self.job_id],
                |row| row.get::<_, bool>(0),
            )
            .unwrap_or(false)
    }
}

fn cancelled_error() -> AppError {
    AppError::Cancelled(
        "Backup cancelled. Files already uploaded were left safely in the cloud.".into(),
    )
}

fn check_cancellation(progress: Option<&ProgressReporter>) -> AppResult<()> {
    if progress.is_some_and(ProgressReporter::cancellation_requested) {
        Err(cancelled_error())
    } else {
        Ok(())
    }
}

fn rclone_progress_percent(line: &str) -> Option<i64> {
    line.split(',').find_map(|part| {
        part.trim()
            .strip_suffix('%')
            .and_then(|value| value.parse::<i64>().ok())
            .map(|value| value.clamp(0, 100))
    })
}

fn clean_rclone_log_line(line: &str) -> String {
    let trimmed = line.trim();
    for level in ["DEBUG", "INFO", "NOTICE", "WARNING", "ERROR"] {
        if let Some(level_index) = trimmed.find(level) {
            let after_level = &trimmed[level_index + level.len()..];
            return after_level.trim_start_matches([' ', ':']).trim().to_owned();
        }
    }
    trimmed.to_owned()
}

fn activity_state_for_line(line: &str, has_progress: bool) -> &'static str {
    if has_progress {
        return "copying";
    }
    let lowercase = line.to_lowercase();
    if lowercase.contains("retry") || lowercase.contains("low level retry") {
        "retrying"
    } else if lowercase.contains("error") || lowercase.contains("failed") {
        "error"
    } else if lowercase.contains("waiting")
        || lowercase.contains("rate limit")
        || lowercase.contains("pacer")
    {
        "waiting"
    } else if lowercase.contains("listing")
        || lowercase.contains("checking")
        || lowercase.contains("scanning")
    {
        "scanning"
    } else if lowercase.contains("copied")
        || lowercase.contains("moved")
        || lowercase.contains("deleted")
    {
        "copying"
    } else {
        "info"
    }
}

fn source_progress_message(source_name: &str, source_number: usize, source_count: usize) -> String {
    if source_count > 1 {
        format!("Copying {source_name} ({source_number} of {source_count})")
    } else {
        format!("Copying {source_name}")
    }
}

fn append_filter_arguments(
    command: &mut Command,
    filters: &EffectiveFilters,
    custom_patterns: &[String],
) {
    for pattern in custom_patterns {
        command.arg("--exclude").arg(pattern);
    }
    if let Some(max_file_size_mib) = filters.max_file_size_mib {
        command
            .arg("--max-size")
            .arg(format!("{max_file_size_mib}M"));
    }
    for extension in &filters.excluded_extensions {
        command
            .arg("--exclude")
            .arg(case_insensitive_extension_pattern(extension));
    }
}

fn case_insensitive_extension_pattern(extension: &str) -> String {
    let mut pattern = String::from("*.");
    for character in extension.chars() {
        if character.is_ascii_alphabetic() {
            pattern.push('[');
            pattern.push(character.to_ascii_lowercase());
            pattern.push(character.to_ascii_uppercase());
            pattern.push(']');
        } else {
            pattern.push(character);
        }
    }
    pattern
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalFileEntry {
    relative_path: String,
    mtime_ns: i64,
    size_bytes: i64,
}

#[derive(Debug, Clone)]
struct SourceScanResult {
    changed_files: Vec<String>,
    scanned_entries: Vec<LocalFileEntry>,
    has_cache: bool,
}

fn matches_exclude_pattern(relative_path: &str, pattern: &str, is_dir: bool) -> bool {
    let rel_norm = relative_path.replace('\\', "/");
    let pat_norm = pattern.replace('\\', "/");
    let rel = rel_norm.trim_start_matches('/');
    let pat = pat_norm.trim();

    if pat.is_empty() {
        return false;
    }

    if rel == pat || (is_dir && format!("{rel}/") == pat) {
        return true;
    }

    if let Some(target) = pat.strip_prefix("**/") {
        let clean_target = target
            .trim_end_matches("/**")
            .trim_end_matches("/*")
            .trim_end_matches('/');
        for segment in rel.split('/') {
            if segment.eq_ignore_ascii_case(clean_target) {
                return true;
            }
        }
        if rel.starts_with(clean_target)
            || rel.contains(&format!("/{clean_target}/"))
            || rel.ends_with(&format!("/{clean_target}"))
        {
            return true;
        }
    }

    if let Some(prefix) = pat.strip_suffix("/**").or_else(|| pat.strip_suffix("/*")) {
        if rel == prefix || rel.starts_with(&format!("{prefix}/")) {
            return true;
        }
    }

    if pat.starts_with("*.") {
        let ext = &pat[2..];
        if let Some(file_ext) = Path::new(rel).extension().and_then(|s| s.to_str()) {
            if file_ext.eq_ignore_ascii_case(ext) {
                return true;
            }
        }
    }

    if pat.contains('*') {
        let parts: Vec<&str> = pat.split('*').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return true;
        }
        let mut search_in = rel;
        let mut all_found = true;
        for (i, part) in parts.iter().enumerate() {
            if i == 0 && !pat.starts_with('*') {
                if !search_in.starts_with(part) {
                    all_found = false;
                    break;
                }
                search_in = &search_in[part.len()..];
            } else if let Some(idx) = search_in.find(part) {
                search_in = &search_in[idx + part.len()..];
            } else {
                all_found = false;
                break;
            }
        }
        if all_found && (pat.ends_with('*') || search_in.is_empty()) {
            return true;
        }
    }

    false
}

fn is_path_excluded(
    relative_path: &str,
    is_dir: bool,
    exclude_patterns: &[String],
    filters: &EffectiveFilters,
    file_size_bytes: Option<u64>,
) -> bool {
    let rel = relative_path.trim_start_matches('/');

    if !is_dir {
        if let (Some(max_mb), Some(size)) = (filters.max_file_size_mib, file_size_bytes) {
            if size > (max_mb as u64) * 1024 * 1024 {
                return true;
            }
        }

        if let Some(ext) = Path::new(rel).extension().and_then(|s| s.to_str()) {
            if filters
                .excluded_extensions
                .iter()
                .any(|e| e.eq_ignore_ascii_case(ext))
            {
                return true;
            }
        }
    }

    for pattern in exclude_patterns {
        if matches_exclude_pattern(rel, pattern, is_dir) {
            return true;
        }
    }

    false
}

fn scan_source_with_cache(
    source_root: &Path,
    job_id: i64,
    source_index: usize,
    db_path: Option<&Path>,
    filters: &EffectiveFilters,
    exclude_patterns: &[String],
) -> AppResult<SourceScanResult> {
    let mut cached_map: HashMap<String, (i64, i64)> = HashMap::new();
    let mut has_cache = false;

    if let Some(db_path) = db_path {
        if let Ok(conn) = connect(db_path) {
            if let Ok(mut stmt) = conn.prepare(
                "SELECT relative_path, mtime_ns, size_bytes FROM file_cache WHERE job_id = ?1 AND source_index = ?2",
            ) {
                if let Ok(rows) = stmt.query_map(params![job_id, source_index as i64], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?),
                    ))
                }) {
                    for row in rows.flatten() {
                        cached_map.insert(row.0, row.1);
                    }
                    has_cache = !cached_map.is_empty();
                }
            }
        }
    }

    let mut scanned_entries = Vec::new();
    let mut changed_files = Vec::new();

    if source_root.is_file() {
        let file_name = source_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        if let Ok(metadata) = source_root.symlink_metadata() {
            if !metadata.file_type().is_symlink() {
                let size = metadata.len() as i64;
                let mtime = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0);
                let entry = LocalFileEntry {
                    relative_path: file_name.clone(),
                    mtime_ns: mtime,
                    size_bytes: size,
                };
                let is_changed = match cached_map.get(&file_name) {
                    Some(&(c_mtime, c_size)) => c_mtime != mtime || c_size != size,
                    None => true,
                };
                if is_changed {
                    changed_files.push(file_name);
                }
                scanned_entries.push(entry);
            }
        }
        return Ok(SourceScanResult {
            changed_files,
            scanned_entries,
            has_cache,
        });
    }

    let mut stack = vec![source_root.to_path_buf()];
    while let Some(current_dir) = stack.pop() {
        let entries = match std::fs::read_dir(&current_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let metadata = match path.symlink_metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                continue;
            }

            let rel_path = match path.strip_prefix(source_root) {
                Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };

            if file_type.is_dir() {
                if is_path_excluded(&rel_path, true, exclude_patterns, filters, None) {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file() {
                let size = metadata.len() as i64;
                if is_path_excluded(&rel_path, false, exclude_patterns, filters, Some(size as u64)) {
                    continue;
                }

                let mtime = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0);

                let entry = LocalFileEntry {
                    relative_path: rel_path.clone(),
                    mtime_ns: mtime,
                    size_bytes: size,
                };

                let is_changed = match cached_map.get(&rel_path) {
                    Some(&(c_mtime, c_size)) => c_mtime != mtime || c_size != size,
                    None => true,
                };

                if is_changed {
                    changed_files.push(rel_path);
                }
                scanned_entries.push(entry);
            }
        }
    }

    Ok(SourceScanResult {
        changed_files,
        scanned_entries,
        has_cache,
    })
}

fn commit_file_cache(
    db_path: &Path,
    job_id: i64,
    source_index: usize,
    entries: &[LocalFileEntry],
) -> AppResult<()> {
    let mut conn = connect(db_path)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute(
        "DELETE FROM file_cache WHERE job_id = ?1 AND source_index = ?2",
        params![job_id, source_index as i64],
    )?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO file_cache (job_id, source_index, relative_path, mtime_ns, size_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for entry in entries {
            stmt.execute(params![
                job_id,
                source_index as i64,
                entry.relative_path,
                entry.mtime_ns,
                entry.size_bytes
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn perform_copy_with_filters(
    job: &SyncJob,
    filters: &EffectiveFilters,
    progress: Option<&ProgressReporter>,
) -> AppResult<CopyOutcome> {
    check_cancellation(progress)?;
    let multiple_sources = job.source_paths.len() > 1;
    let source_count = job.source_paths.len();
    let mut completed = Vec::new();
    let mut failures = Vec::new();
    let stamp = backup_stamp();
    let (mode_destination, full_completed) = match job.backup_mode.as_str() {
        "full" => (
            child_cloud_destination(&job.destination, &format!("Full/{stamp}")),
            true,
        ),
        "differential" if job.last_full_at.is_none() => (
            child_cloud_destination(&job.destination, &format!("Baseline/{stamp}")),
            true,
        ),
        "differential" => (
            child_cloud_destination(&job.destination, &format!("Differential/{stamp}")),
            false,
        ),
        _ => (job.destination.clone(), false),
    };
    if supports_previous_files(job) {
        if let Some(progress) = progress {
            progress.activity("preparing", "Preparing the Previous files safety folder.");
        }
        ensure_version_history(job)?;
        check_cancellation(progress)?;
    }
    let active_filter_count = job.exclude_patterns.len()
        + filters.excluded_extensions.len()
        + usize::from(filters.max_file_size_mib.is_some());
    if active_filter_count > 0 {
        if let Some(progress) = progress {
            progress.activity(
                "preparing",
                &format!(
                    "Using {active_filter_count} file filter{} to skip files that do not need backing up.",
                    if active_filter_count == 1 {
                        ""
                    } else {
                        "s"
                    }
                ),
            );
        }
    }

    for (source_index, source_path) in job.source_paths.iter().enumerate() {
        check_cancellation(progress)?;
        let source = Path::new(source_path);
        let source_name = source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| source_path.clone());
        let progress_message =
            source_progress_message(&source_name, source_index + 1, source_count);
        if let Some(progress) = progress {
            progress.activity(
                "scanning",
                &format!("Checking {source_name} for files that need backing up."),
            );
            progress.report(
                ((source_index * 100) / source_count.max(1)) as i64,
                &progress_message,
            );
        }
        if !source.exists() {
            let message =
                format!("{source_name}: source is unavailable; reconnect its drive and try again");
            if let Some(progress) = progress {
                progress.activity("error", &message);
            }
            failures.push(message);
            continue;
        }
        let destination = if multiple_sources {
            child_cloud_destination(&mode_destination, &source_name)
        } else {
            mode_destination.clone()
        };

        let db_path = progress.map(|p| p.db_path.as_path());
        let use_fast_cache = job.backup_mode == "incremental" && db_path.is_some();
        let scan_result = if use_fast_cache {
            Some(scan_source_with_cache(
                source,
                job.id,
                source_index,
                db_path,
                filters,
                &job.exclude_patterns,
            )?)
        } else {
            None
        };

        let mut manifest_path_to_clean: Option<PathBuf> = None;

        if let Some(ref scan) = scan_result {
            if scan.has_cache && scan.changed_files.is_empty() {
                if let Some(progress) = progress {
                    let completed_percent =
                        (((source_index + 1) * 100) / source_count.max(1)) as i64;
                    progress.report(completed_percent.min(99), &progress_message);
                    progress.activity(
                        "success",
                        &format!("{source_name}: Up to date (0 files modified)."),
                    );
                }
                completed.push(format!("{source_name} → {destination}: Up to date (0 files modified)"));
                continue;
            }

            if scan.has_cache && !scan.changed_files.is_empty() {
                if let Some(progress) = progress {
                    progress.activity(
                        "preparing",
                        &format!(
                            "{source_name}: Found {} modified file{} to back up.",
                            scan.changed_files.len(),
                            if scan.changed_files.len() == 1 { "" } else { "s" }
                        ),
                    );
                }
                let temp_manifest = std::env::temp_dir().join(format!(
                    "cloudfolder_manifest_{}_{}_{}.txt",
                    job.id,
                    source_index,
                    std::process::id()
                ));
                if std::fs::write(&temp_manifest, scan.changed_files.join("\n")).is_ok() {
                    manifest_path_to_clean = Some(temp_manifest);
                }
            }
        }

        let mut command = Command::new("rclone");
        command
            .arg(if job.backup_mode == "mirror" {
                "sync"
            } else {
                "copy"
            })
            .arg(source_path)
            .arg(&destination)
            .arg("--create-empty-src-dirs")
            .arg("--stats-one-line")
            .arg("--stats=1s")
            .arg("--stats-log-level=NOTICE")
            .arg("--log-level=INFO")
            .arg("--retries=3")
            .arg("--transfers=8")
            .arg("--checkers=16")
            .arg("--fast-list")
            .arg("--drive-chunk-size=64M")
            .arg("--drive-pacer-min-sleep=10ms")
            .arg("--drive-pacer-burst=200")
            .arg("--drive-use-trash=false")
            .arg("--skip-links")
            .arg("--use-mmap")
            .arg("--buffer-size=32M")
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(ref temp_manifest) = manifest_path_to_clean {
            command.arg("--files-from-raw").arg(temp_manifest);
        }
        append_filter_arguments(&mut command, filters, &job.exclude_patterns);
        if supports_previous_files(job) {
            let snapshot = version_snapshot_destination(job, &stamp)?;
            let backup_directory = if multiple_sources {
                child_cloud_destination(&snapshot, &source_name)
            } else {
                snapshot
            };
            command.arg("--backup-dir").arg(backup_directory);
        }
        if job.backup_mode == "differential" {
            if let Some(last_full_at) = &job.last_full_at {
                if let Ok(baseline) = chrono::DateTime::parse_from_rfc3339(last_full_at) {
                    let age_seconds = (Utc::now() - baseline.with_timezone(&Utc))
                        .num_seconds()
                        .max(60)
                        + 60;
                    command.arg("--max-age").arg(format!("{age_seconds}s"));
                }
            }
        }
        check_cancellation(progress)?;
        let mut child = command.spawn().map_err(|error| {
            if let Some(ref path) = manifest_path_to_clean {
                let _ = std::fs::remove_file(path);
            }
            AppError::Transfer(format!(
                "Could not start rclone: {error}. Install rclone and try again."
            ))
        })?;
        if let Some(progress) = progress {
            progress.activity(
                "copying",
                &format!("Cloud connection opened for {source_name}."),
            );
        }
        let stderr = child.stderr.take().ok_or_else(|| {
            if let Some(ref path) = manifest_path_to_clean {
                let _ = std::fs::remove_file(path);
            }
            AppError::Transfer("CloudFolder could not read rclone progress".into())
        })?;
        let mut messages = Vec::new();
        for line in BufReader::new(stderr).lines() {
            match line {
                Ok(line) => {
                    let source_percent = rclone_progress_percent(&line);
                    let clean_line = clean_rclone_log_line(&line);
                    if let Some(source_percent) = source_percent {
                        let overall_percent = (((source_index as f64
                            + source_percent as f64 / 100.0)
                            / source_count.max(1) as f64)
                            * 100.0)
                            .round() as i64;
                        if let Some(progress) = progress {
                            progress.report(overall_percent.min(99), &progress_message);
                            progress.activity(
                                activity_state_for_line(&line, true),
                                &format!("{source_name}: {clean_line}"),
                            );
                        }
                    } else if !line.trim().is_empty() {
                        if let Some(progress) = progress {
                            progress.activity(
                                activity_state_for_line(&line, false),
                                &format!("{source_name}: {clean_line}"),
                            );
                        }
                        messages.push(line);
                    }
                }
                Err(error) => {
                    let message = format!("Could not read transfer details: {error}");
                    if let Some(progress) = progress {
                        progress.activity("error", &message);
                    }
                    messages.push(message);
                }
            }
            if progress.is_some_and(ProgressReporter::cancellation_requested) {
                if let Some(ref path) = manifest_path_to_clean {
                    let _ = std::fs::remove_file(path);
                }
                if let Some(progress) = progress {
                    progress.activity("cancelled", "Stopping the active cloud transfer safely.");
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(cancelled_error());
            }
        }
        if progress.is_some_and(ProgressReporter::cancellation_requested) {
            if let Some(ref path) = manifest_path_to_clean {
                let _ = std::fs::remove_file(path);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(cancelled_error());
        }
        let status = child.wait()?;
        if let Some(ref path) = manifest_path_to_clean {
            let _ = std::fs::remove_file(path);
        }
        if status.success() {
            if let (Some(db_path), Some(scan)) = (db_path, scan_result) {
                let _ = commit_file_cache(db_path, job.id, source_index, &scan.scanned_entries);
            }
            if let Some(progress) = progress {
                let completed_percent = (((source_index + 1) * 100) / source_count.max(1)) as i64;
                progress.report(completed_percent.min(99), &progress_message);
                progress.activity("success", &format!("Finished backing up {source_name}."));
            }
            let message = if messages.is_empty() {
                "Backup completed".into()
            } else {
                compact_output(messages.join("\n").as_bytes(), &[])
            };
            completed.push(format!("{source_name} → {destination}: {message}"));
        } else {
            let message = if messages.is_empty() {
                "rclone stopped before the backup finished".into()
            } else {
                compact_output(messages.join("\n").as_bytes(), &[])
            };
            let failure = format!("{source_name}: {message}");
            if let Some(progress) = progress {
                progress.activity("error", &failure);
            }
            failures.push(failure);
        }
    }

    if failures.is_empty() {
        check_cancellation(progress)?;
        if supports_previous_files(job) {
            if let Some(progress) = progress {
                progress.activity("preparing", "Checking the Previous files retention limit.");
            }
            if let Err(error) = prune_version_history(job) {
                let warning =
                    format!("Backup completed, but old Previous files need attention: {error}");
                if let Some(progress) = progress {
                    progress.activity("error", &warning);
                }
                completed.push(warning);
            }
        }
        Ok(CopyOutcome {
            message: completed.join("\n"),
            full_completed,
        })
    } else {
        let partial_note = if completed.is_empty() {
            String::new()
        } else {
            format!("{} source(s) completed. ", completed.len())
        };
        Err(AppError::Transfer(format!(
            "{partial_note}{} source(s) failed:\n{}",
            failures.len(),
            failures.join("\n")
        )))
    }
}

#[cfg(test)]
fn perform_copy(job: &SyncJob, progress: Option<&ProgressReporter>) -> AppResult<CopyOutcome> {
    let settings = AppSettings {
        max_file_size_mib: None,
        excluded_extensions: Vec::new(),
    };
    let filters = resolve_filter_values(
        &job.size_filter_mode,
        job.max_file_size_mib,
        &job.extension_filter_mode,
        &job.excluded_extensions,
        &settings,
    )?;
    perform_copy_with_filters(job, &filters, progress)
}

fn child_cloud_destination(destination: &str, child_name: &str) -> String {
    if destination.ends_with(':') || destination.ends_with('/') {
        format!("{destination}{child_name}")
    } else {
        format!("{destination}/{child_name}")
    }
}

fn claim_job_for_run(
    db_path: &Path,
    job_id: i64,
) -> AppResult<(SyncJob, EffectiveFilters, String)> {
    let mut connection = connect(db_path)?;
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let started_at = now_string();
    let claimed = transaction.execute(
        "UPDATE jobs
         SET status = 'running', last_message = NULL,
             progress_percent = 0, progress_message = 'Getting ready…',
             cancel_requested = 0
         WHERE id = ?1 AND status != 'running'",
        [job_id],
    )?;
    if claimed == 0 {
        return Err(AppError::Validation(
            "This backup is already running in the background".into(),
        ));
    }
    let job = get_job(&transaction, job_id)?;
    let app_settings = load_app_settings(&transaction)?;
    let effective_filters = resolve_filter_values(
        &job.size_filter_mode,
        job.max_file_size_mib,
        &job.extension_filter_mode,
        &job.excluded_extensions,
        &app_settings,
    )?;
    transaction.execute("DELETE FROM backup_activity WHERE job_id = ?1", [job_id])?;
    record_activity(
        &transaction,
        job_id,
        "preparing",
        "Backup started. Getting everything ready.",
    )?;
    transaction.commit()?;
    Ok((job, effective_filters, started_at))
}

async fn execute_job(job_id: i64, state: AppState) -> AppResult<RunRecord> {
    {
        let mut running = state
            .running_jobs
            .lock()
            .map_err(|_| AppError::Transfer("The scheduler lock is unavailable".into()))?;
        if !running.insert(job_id) {
            return Err(AppError::Validation(
                "This backup is already running".into(),
            ));
        }
    }

    let result = async {
        let (job, effective_filters, started_at) = claim_job_for_run(&state.db_path, job_id)?;

        let job_for_copy = job.clone();
        let progress = ProgressReporter {
            db_path: state.db_path.clone(),
            job_id,
        };
        let worker_progress = progress.clone();
        let copy_result = tokio::task::spawn_blocking(move || {
            perform_copy_with_filters(&job_for_copy, &effective_filters, Some(&worker_progress))
        })
        .await
        .map_err(|error| AppError::Transfer(format!("Backup worker failed: {error}")))?;

        let finished_at = now_string();
        let (run_status, job_status, message, full_completed) = match copy_result {
            Ok(_) if progress.cancellation_requested() => {
                ("cancelled", "ready", cancelled_error().to_string(), false)
            }
            Ok(outcome) => (
                "success",
                "success",
                outcome.message,
                outcome.full_completed,
            ),
            Err(AppError::Cancelled(message)) => ("cancelled", "ready", message, false),
            Err(error) => ("error", "error", error.to_string(), false),
        };
        progress.activity(
            run_status,
            if run_status == "success" {
                "Backup finished successfully."
            } else {
                &message
            },
        );
        let connection = connect(&state.db_path)?;
        connection.execute(
            "UPDATE jobs
             SET status = ?1, last_message = ?2, last_run_at = ?3,
                 next_run_at = ?4,
                 last_full_at = CASE WHEN ?5 = 1 THEN ?3 ELSE last_full_at END,
                 progress_percent = CASE WHEN ?1 = 'success' THEN 100 ELSE 0 END,
                 progress_message = NULL, cancel_requested = 0
             WHERE id = ?6",
            params![
                job_status,
                message,
                finished_at,
                add_minutes(job.interval_minutes),
                i64::from(full_completed),
                job_id
            ],
        )?;
        connection.execute(
            "INSERT INTO runs
                (job_id, started_at, finished_at, status, message)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![job_id, started_at, finished_at, run_status, message],
        )?;
        Ok(RunRecord {
            id: connection.last_insert_rowid(),
            job_id,
            started_at,
            finished_at,
            status: run_status.into(),
            message,
        })
    }
    .await;

    if let Ok(mut running) = state.running_jobs.lock() {
        running.remove(&job_id);
    }
    result
}

#[tauri::command]
async fn run_job(job_id: i64, state: State<'_, AppState>) -> AppResult<RunRecord> {
    execute_job(job_id, state.inner().clone()).await
}

#[tauri::command]
fn list_version_snapshots(
    job_id: i64,
    state: State<'_, AppState>,
) -> AppResult<Vec<VersionSnapshot>> {
    let connection = connect(&state.db_path)?;
    let job = get_job(&connection, job_id)?;
    version_snapshots_for_job(&job)
}

#[tauri::command]
fn list_version_files(
    job_id: i64,
    snapshot_name: String,
    state: State<'_, AppState>,
) -> AppResult<Vec<VersionFile>> {
    validate_snapshot_name(&snapshot_name)?;
    let connection = connect(&state.db_path)?;
    let job = get_job(&connection, job_id)?;
    if !supports_previous_files(&job) {
        return Ok(Vec::new());
    }
    let output = Command::new("rclone")
        .arg("lsjson")
        .arg(version_snapshot_destination(&job, &snapshot_name)?)
        .arg("--recursive")
        .arg("--files-only")
        .arg("--no-mimetype")
        .output()
        .map_err(|error| {
            AppError::Transfer(format!("Could not read the previous files: {error}"))
        })?;
    if !output.status.success() {
        return Err(AppError::Transfer(format!(
            "CloudFolder could not read that previous backup. {}",
            compact_output(&output.stdout, &output.stderr)
        )));
    }
    let mut files = serde_json::from_slice::<Vec<RcloneVersionItem>>(&output.stdout)
        .map_err(|error| {
            AppError::Transfer(format!(
                "The previous backup returned an unreadable file list: {error}"
            ))
        })?
        .into_iter()
        .map(|item| VersionFile {
            path: if item.path.is_empty() {
                item.name
            } else {
                item.path
            },
            size: item.size,
            modified_at: item.modified_at,
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|file| file.path.to_lowercase());
    Ok(files)
}

fn unique_restored_path(destination: &Path, file_name: &str) -> PathBuf {
    let first_choice = destination.join(file_name);
    if !first_choice.exists() {
        return first_choice;
    }
    let file_path = Path::new(file_name);
    let stem = file_path
        .file_stem()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "restored-file".into());
    let extension = file_path
        .extension()
        .map(|value| value.to_string_lossy().into_owned());
    for copy_number in 1..10_000 {
        let candidate_name = match &extension {
            Some(extension) => format!("{stem} (restored {copy_number}).{extension}"),
            None => format!("{stem} (restored {copy_number})"),
        };
        let candidate = destination.join(candidate_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    destination.join(format!("{stem} (restored {})", backup_stamp()))
}

fn restore_version_file_for_job(
    job: &SyncJob,
    snapshot_name: &str,
    file_path: &str,
    destination: &Path,
) -> AppResult<RestoredVersion> {
    validate_snapshot_name(snapshot_name)?;
    validate_version_file_path(file_path)?;
    if !destination.is_absolute() || !destination.is_dir() {
        return Err(AppError::Validation(
            "Choose a folder on this computer for the restored file".into(),
        ));
    }
    if !supports_previous_files(job) {
        return Err(AppError::Validation(
            "Previous files are not turned on for this backup".into(),
        ));
    }
    let file_name = Path::new(file_path)
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .ok_or_else(|| AppError::Validation("That previous file has no name".into()))?;
    let restored_path = unique_restored_path(destination, &file_name);
    let remote_file = child_cloud_destination(
        &version_snapshot_destination(job, snapshot_name)?,
        file_path,
    );
    let output = Command::new("rclone")
        .arg("copyto")
        .arg(remote_file)
        .arg(&restored_path)
        .arg("--drive-chunk-size=64M")
        .arg("--drive-pacer-min-sleep=10ms")
        .arg("--drive-pacer-burst=200")
        .arg("--use-mmap")
        .arg("--buffer-size=32M")
        .output()
        .map_err(|error| {
            AppError::Transfer(format!("Could not start restoring the file: {error}"))
        })?;
    if !output.status.success() {
        return Err(AppError::Transfer(format!(
            "CloudFolder could not restore that file. {}",
            compact_output(&output.stdout, &output.stderr)
        )));
    }
    Ok(RestoredVersion {
        path: restored_path.to_string_lossy().into_owned(),
        message: format!("{file_name} was restored without replacing any files on this computer."),
    })
}

#[tauri::command]
fn restore_version_file(
    job_id: i64,
    snapshot_name: String,
    file_path: String,
    destination_dir: String,
    state: State<'_, AppState>,
) -> AppResult<RestoredVersion> {
    let connection = connect(&state.db_path)?;
    let job = get_job(&connection, job_id)?;
    restore_version_file_for_job(
        &job,
        &snapshot_name,
        &file_path,
        Path::new(&destination_dir),
    )
}

#[tauri::command]
fn job_history(job_id: i64, state: State<'_, AppState>) -> AppResult<Vec<RunRecord>> {
    let connection = connect(&state.db_path)?;
    get_job(&connection, job_id)?;
    let mut statement = connection.prepare(
        "SELECT id, job_id, started_at, finished_at, status, message
         FROM runs WHERE job_id = ?1 ORDER BY id DESC LIMIT 20",
    )?;
    let runs = statement
        .query_map([job_id], |row| {
            Ok(RunRecord {
                id: row.get(0)?,
                job_id: row.get(1)?,
                started_at: row.get(2)?,
                finished_at: row.get(3)?,
                status: row.get(4)?,
                message: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(runs)
}

fn query_job_activity(connection: &Connection, job_id: i64) -> AppResult<Vec<ActivityLogEntry>> {
    let mut statement = connection.prepare(
        "SELECT id, job_id, occurred_at, state, message
         FROM (
             SELECT id, job_id, occurred_at, state, message
             FROM backup_activity
             WHERE job_id = ?1
             ORDER BY id DESC
             LIMIT 500
         )
         ORDER BY id ASC",
    )?;
    let entries = statement
        .query_map([job_id], |row| {
            Ok(ActivityLogEntry {
                id: row.get(0)?,
                job_id: row.get(1)?,
                occurred_at: row.get(2)?,
                state: row.get(3)?,
                message: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(entries)
}

#[tauri::command]
fn job_activity(job_id: i64, state: State<'_, AppState>) -> AppResult<Vec<ActivityLogEntry>> {
    let connection = connect(&state.db_path)?;
    get_job(&connection, job_id)?;
    query_job_activity(&connection, job_id)
}

fn query_error_logs(connection: &Connection) -> AppResult<Vec<ErrorLog>> {
    let mut statement = connection.prepare(
        "SELECT runs.id, runs.job_id, jobs.name, runs.started_at,
                runs.finished_at, runs.message
         FROM runs
         INNER JOIN jobs ON jobs.id = runs.job_id
         WHERE runs.status = 'error'
         ORDER BY runs.id DESC
         LIMIT 100",
    )?;
    let logs = statement
        .query_map([], |row| {
            Ok(ErrorLog {
                id: row.get(0)?,
                job_id: row.get(1)?,
                job_name: row.get(2)?,
                started_at: row.get(3)?,
                finished_at: row.get(4)?,
                message: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(logs)
}

#[tauri::command]
fn list_error_logs(state: State<'_, AppState>) -> AppResult<Vec<ErrorLog>> {
    let connection = connect(&state.db_path)?;
    query_error_logs(&connection)
}

fn configured_remotes() -> AppResult<Vec<String>> {
    Ok(providers::remote_list()?
        .into_iter()
        .map(|remote| remote.name)
        .collect())
}

#[tauri::command]
fn list_remotes() -> AppResult<Vec<RemoteInfo>> {
    providers::remote_list()
}

#[tauri::command]
fn list_providers() -> Vec<providers::ProviderInfo> {
    providers::provider_infos()
}

/// Signs in to a browser-based provider. Blocking because rclone waits for the
/// whole OAuth round trip, so it runs off the interface thread.
#[tauri::command]
async fn connect_provider(provider_id: String) -> AppResult<String> {
    tauri::async_runtime::spawn_blocking(move || providers::connect_browser(&provider_id))
        .await
        .map_err(|error| AppError::Transfer(format!("Cloud sign-in stopped: {error}")))?
}

/// Creates a remote from credentials typed into a form.
#[tauri::command]
async fn connect_provider_with_fields(
    provider_id: String,
    fields: HashMap<String, String>,
) -> AppResult<String> {
    tauri::async_runtime::spawn_blocking(move || providers::connect_fields(&provider_id, fields))
        .await
        .map_err(|error| AppError::Transfer(format!("Cloud setup stopped: {error}")))?
}

/// Forgets a cloud connection, refusing while any backup job still points at it.
#[tauri::command]
fn disconnect_remote(state: State<'_, AppState>, remote: String) -> AppResult<()> {
    let connection = connect(&state.db_path)?;
    let mut statement = connection.prepare("SELECT name, destination FROM jobs")?;
    let users: Vec<String> = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|(_, destination)| destination.starts_with(&remote))
        .map(|(name, _)| name)
        .collect();
    if !users.is_empty() {
        return Err(AppError::Validation(format!(
            "These backups still use this cloud account: {}. Point them somewhere else first.",
            users.join(", ")
        )));
    }
    providers::delete_remote(&remote)
}

#[tauri::command]
fn list_cloud_folders(remote: String, path: String) -> AppResult<Vec<CloudFolderEntry>> {
    ensure_configured_remote(&remote)?;
    let clean_path = clean_cloud_path(&path)?;
    let destination = cloud_destination(&remote, &clean_path);
    let output = Command::new("rclone")
        .arg("lsjson")
        .arg(destination)
        .arg("--dirs-only")
        .arg("--fast-list")
        .arg("--no-modtime")
        .arg("--no-mimetype")
        .output()
        .map_err(|error| {
            AppError::Transfer(format!(
                "CloudFolder could not look inside {}: {error}",
                remote_label(&remote)
            ))
        })?;
    if !output.status.success() {
        return Err(AppError::Transfer(format!(
            "{} could not open this folder. {}",
            remote_label(&remote),
            compact_output(&output.stdout, &output.stderr)
        )));
    }

    let mut folders: Vec<CloudFolderEntry> =
        serde_json::from_slice::<Vec<RcloneListItem>>(&output.stdout)
            .map_err(|error| {
                AppError::Transfer(format!(
                    "{} returned a folder list CloudFolder could not read: {error}",
                    remote_label(&remote)
                ))
            })?
            .into_iter()
            .map(|item| {
                let path = join_cloud_path(&clean_path, &item.name);
                CloudFolderEntry {
                    name: item.name,
                    path,
                }
            })
            .collect();
    folders.sort_by_key(|folder| folder.name.to_lowercase());
    Ok(folders)
}

#[tauri::command]
fn create_cloud_folder(remote: String, path: String, name: String) -> AppResult<String> {
    ensure_configured_remote(&remote)?;
    let clean_path = clean_cloud_path(&path)?;
    let clean_name = name.trim();
    if clean_name.is_empty() {
        return Err(AppError::Validation("Give the new folder a name".into()));
    }
    if clean_name == "." || clean_name == ".." || clean_name.contains('/') {
        return Err(AppError::Validation(
            "Folder names cannot be a dot or contain a slash".into(),
        ));
    }
    if clean_name.chars().any(char::is_control) {
        return Err(AppError::Validation(
            "Folder names cannot contain control characters".into(),
        ));
    }

    let new_path = join_cloud_path(&clean_path, clean_name);
    let output = Command::new("rclone")
        .arg("mkdir")
        .arg(cloud_destination(&remote, &new_path))
        .output()
        .map_err(|error| {
            AppError::Transfer(format!(
                "CloudFolder could not create that folder in {}: {error}",
                remote_label(&remote)
            ))
        })?;
    if !output.status.success() {
        return Err(AppError::Transfer(format!(
            "{} could not create that folder. {}",
            remote_label(&remote),
            compact_output(&output.stdout, &output.stderr)
        )));
    }
    Ok(new_path)
}

/// The friendly name of a connected remote, for use in messages. Falls back to
/// the remote's own name when it cannot be looked up, so a failing lookup never
/// hides the error the user actually needs to read.
fn remote_label(remote: &str) -> String {
    providers::remote_list()
        .ok()
        .and_then(|remotes| {
            remotes
                .into_iter()
                .find(|candidate| candidate.name == remote)
                .map(|candidate| candidate.label)
        })
        .unwrap_or_else(|| remote.trim_end_matches(':').to_owned())
}

fn ensure_configured_remote(remote: &str) -> AppResult<()> {
    if !remote.ends_with(':')
        || !configured_remotes()?
            .iter()
            .any(|configured| configured == remote)
    {
        return Err(AppError::Validation(
            "Choose a connected cloud account first".into(),
        ));
    }
    Ok(())
}

fn clean_cloud_path(path: &str) -> AppResult<String> {
    let clean = path.trim_matches('/').trim().to_owned();
    if clean.split('/').any(|part| part == "." || part == "..") {
        return Err(AppError::Validation(
            "That cloud folder path is not valid".into(),
        ));
    }
    if clean.chars().any(char::is_control) {
        return Err(AppError::Validation(
            "That cloud folder path contains unsupported characters".into(),
        ));
    }
    Ok(clean)
}

fn join_cloud_path(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_owned()
    } else {
        format!("{parent}/{child}")
    }
}

fn cloud_destination(remote: &str, path: &str) -> String {
    format!("{remote}{path}")
}

#[tauri::command]
fn open_rclone_config() -> AppResult<()> {
    let candidates: &[(&str, &[&str])] = &[
        ("x-terminal-emulator", &["-e", "rclone", "config"]),
        ("gnome-terminal", &["--", "rclone", "config"]),
        ("konsole", &["-e", "rclone", "config"]),
        ("xterm", &["-e", "rclone", "config"]),
    ];
    for (program, arguments) in candidates {
        match Command::new(program).args(*arguments).spawn() {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(AppError::Io(error)),
        }
    }
    Err(AppError::Validation(
        "No supported terminal was found. Run `rclone config` in a terminal.".into(),
    ))
}

const SUPPORT_URL: &str = "https://ko-fi.com/ryanrobertolson";

#[tauri::command]
fn open_support_page() -> AppResult<()> {
    Command::new("xdg-open")
        .arg(SUPPORT_URL)
        .spawn()
        .map(|_| ())
        .map_err(|error| {
            AppError::Io(std::io::Error::new(
                error.kind(),
                format!("Could not open the Ko-fi page: {error}"),
            ))
        })
}

const RELEASE_API_URL: &str =
    "https://api.github.com/repos/Ryanrobertolson/cloudfolder-sync/releases/latest";

fn github_client() -> AppResult<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(format!("CloudFolder-Sync/{}", env!("CARGO_PKG_VERSION")))
        .timeout(StdDuration::from_secs(30))
        .build()
        .map_err(|error| AppError::Transfer(format!("Could not prepare the update check: {error}")))
}

fn package_type() -> &'static str {
    if std::env::var_os("APPIMAGE").is_some() {
        "appimage"
    } else {
        "deb"
    }
}

fn release_asset<'a>(release: &'a GitHubRelease, kind: &str) -> Option<&'a GitHubAsset> {
    let expected_suffix = if kind == "appimage" {
        ".AppImage"
    } else {
        "_amd64.deb"
    };
    release
        .assets
        .iter()
        .find(|asset| asset.name.ends_with(expected_suffix))
}

#[tauri::command]
fn check_for_updates() -> AppResult<UpdateInfo> {
    let release = github_client()?
        .get(RELEASE_API_URL)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| {
            AppError::Transfer(format!(
                "CloudFolder could not check GitHub for updates: {error}"
            ))
        })?
        .json::<GitHubRelease>()
        .map_err(|error| {
            AppError::Transfer(format!(
                "GitHub returned an unreadable update response: {error}"
            ))
        })?;
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|error| AppError::Validation(format!("Invalid app version: {error}")))?;
    let latest_text = release.tag_name.trim_start_matches(['v', 'V']);
    let latest = semver::Version::parse(latest_text).map_err(|error| {
        AppError::Transfer(format!(
            "The latest GitHub release has an invalid version: {error}"
        ))
    })?;
    let kind = package_type();
    let asset = release_asset(&release, kind);
    let asset_name = asset.map(|value| value.name.clone());
    let download_url = asset.map(|value| value.browser_download_url.clone());
    let download_size = asset.map(|value| value.size);
    Ok(UpdateInfo {
        available: latest > current,
        current_version: current.to_string(),
        latest_version: latest.to_string(),
        title: release
            .name
            .unwrap_or_else(|| format!("CloudFolder Sync {latest}")),
        notes: release
            .body
            .unwrap_or_else(|| "No release notes were provided.".into()),
        release_url: release.html_url,
        published_at: release.published_at,
        asset_name,
        download_url,
        download_size,
        package_type: kind.into(),
    })
}

fn downloads_directory() -> AppResult<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        AppError::Validation("CloudFolder could not find your home folder".into())
    })?;
    let downloads = PathBuf::from(home).join("Downloads");
    std::fs::create_dir_all(&downloads)?;
    Ok(downloads)
}

fn validate_update_download(download_url: &str, asset_name: &str) -> AppResult<reqwest::Url> {
    let url = reqwest::Url::parse(download_url)
        .map_err(|_| AppError::Validation("The update download link is invalid".into()))?;
    if url.scheme() != "https" || url.host_str() != Some("github.com") {
        return Err(AppError::Validation(
            "CloudFolder only downloads updates directly from GitHub".into(),
        ));
    }
    let file_name = Path::new(asset_name)
        .file_name()
        .and_then(|name| name.to_str());
    if file_name != Some(asset_name)
        || !(asset_name.ends_with("_amd64.deb") || asset_name.ends_with(".AppImage"))
    {
        return Err(AppError::Validation(
            "The update has an unsupported file name".into(),
        ));
    }
    Ok(url)
}

#[tauri::command]
fn download_update(download_url: String, asset_name: String) -> AppResult<DownloadedUpdate> {
    let url = validate_update_download(&download_url, &asset_name)?;
    let mut response = github_client()?
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| AppError::Transfer(format!("Could not download the update: {error}")))?;
    let expected_length = response.content_length();
    if expected_length.unwrap_or(0) > 500 * 1024 * 1024 {
        return Err(AppError::Validation(
            "The update file is unexpectedly large".into(),
        ));
    }
    let destination = downloads_directory()?.join(&asset_name);
    let temporary = destination.with_file_name(format!("{asset_name}.part"));
    let mut file = std::fs::File::create(&temporary)?;
    let downloaded_bytes = std::io::copy(&mut response, &mut file)
        .map_err(|error| AppError::Transfer(format!("Could not save the update: {error}")))?;
    drop(file);
    if expected_length.is_some_and(|length| length != downloaded_bytes) {
        let _ = std::fs::remove_file(&temporary);
        return Err(AppError::Transfer(
            "The update download stopped before it was complete".into(),
        ));
    }
    std::fs::rename(&temporary, &destination)?;

    if asset_name.ends_with(".AppImage") {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&destination)?.permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&destination, permissions)?;
        }
        Command::new("xdg-open")
            .arg(destination.parent().unwrap_or(Path::new(".")))
            .spawn()
            .map_err(|error| {
                AppError::Transfer(format!("Could not open the Downloads folder: {error}"))
            })?;
        Ok(DownloadedUpdate {
            path: destination.to_string_lossy().into_owned(),
            instructions:
                "The new AppImage is in Downloads. Close CloudFolder, then open the new AppImage."
                    .into(),
        })
    } else {
        Command::new("xdg-open")
            .arg(&destination)
            .spawn()
            .map_err(|error| {
                AppError::Transfer(format!("Could not open Ubuntu's installer: {error}"))
            })?;
        Ok(DownloadedUpdate {
            path: destination.to_string_lossy().into_owned(),
            instructions:
                "Ubuntu's installer is open. Press Install, enter your password, then reopen CloudFolder."
                    .into(),
        })
    }
}

#[tauri::command]
fn open_release_page(release_url: String) -> AppResult<()> {
    let url = reqwest::Url::parse(&release_url)
        .map_err(|_| AppError::Validation("The release page link is invalid".into()))?;
    if url.scheme() != "https" || url.host_str() != Some("github.com") {
        return Err(AppError::Validation(
            "CloudFolder only opens GitHub release pages".into(),
        ));
    }
    Command::new("xdg-open")
        .arg(url.as_str())
        .spawn()
        .map(|_| ())
        .map_err(|error| {
            AppError::Io(std::io::Error::new(
                error.kind(),
                format!("Could not open the GitHub release page: {error}"),
            ))
        })
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[derive(Debug, Serialize)]
struct BackgroundServiceStatus {
    installed: bool,
    enabled: bool,
    active: bool,
    message: String,
}

fn systemd_user_directory() -> AppResult<PathBuf> {
    if let Some(config_home) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(config_home).join("systemd/user"));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| {
        AppError::Validation("CloudFolder could not find your home folder".into())
    })?;
    Ok(PathBuf::from(home).join(".config/systemd/user"))
}

fn systemd_quote(value: &Path) -> String {
    format!(
        "\"{}\"",
        value
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    )
}

fn background_service_unit(executable: &Path, database: &Path) -> String {
    format!(
        "[Unit]\n\
Description=CloudFolder background backup scheduler\n\
Wants=network-online.target\n\
After=network-online.target\n\n\
[Service]\n\
Type=simple\n\
Environment=CLOUDFOLDER_VERSION={}\n\
ExecStart={} --service --database {}\n\
Restart=always\n\
RestartSec=15\n\n\
[Install]\n\
WantedBy=default.target\n",
        env!("CARGO_PKG_VERSION"),
        systemd_quote(executable),
        systemd_quote(database)
    )
}

fn service_executable() -> AppResult<PathBuf> {
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        return Ok(PathBuf::from(appimage));
    }
    Ok(std::env::current_exe()?)
}

fn systemctl_success(arguments: &[&str]) -> bool {
    Command::new("systemctl")
        .arg("--user")
        .args(arguments)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn background_service_state() -> AppResult<BackgroundServiceStatus> {
    let unit_path = systemd_user_directory()?.join("cloudfolder-sync.service");
    let installed = unit_path.exists();
    let enabled = systemctl_success(&["is-enabled", "cloudfolder-sync.service"]);
    let active = systemctl_success(&["is-active", "cloudfolder-sync.service"]);
    let message = if active {
        "Backups continue after the app closes while you are signed in.".into()
    } else if installed {
        "The background service is installed but is not running.".into()
    } else {
        "The background service has not been installed yet.".into()
    };
    Ok(BackgroundServiceStatus {
        installed,
        enabled,
        active,
        message,
    })
}

fn install_background_service(database: &Path) -> AppResult<BackgroundServiceStatus> {
    let unit_directory = systemd_user_directory()?;
    std::fs::create_dir_all(&unit_directory)?;
    let unit_path = unit_directory.join("cloudfolder-sync.service");
    let unit = background_service_unit(&service_executable()?, database);
    let unit_changed = std::fs::read_to_string(&unit_path)
        .map(|existing| existing.as_str() != unit.as_str())
        .unwrap_or(true);
    let was_active = systemctl_success(&["is-active", "cloudfolder-sync.service"]);
    if unit_changed {
        std::fs::write(&unit_path, unit)?;
    }

    let mut commands = Vec::new();
    if unit_changed {
        commands.push(vec!["daemon-reload"]);
    }
    commands.push(vec!["enable", "--now", "cloudfolder-sync.service"]);
    if was_active && unit_changed {
        commands.push(vec!["restart", "cloudfolder-sync.service"]);
    }
    for arguments in commands {
        let output = Command::new("systemctl")
            .arg("--user")
            .args(&arguments)
            .output()
            .map_err(|error| {
                AppError::Transfer(format!("Could not start the background service: {error}"))
            })?;
        if !output.status.success() {
            return Err(AppError::Transfer(format!(
                "Could not start the background service: {}",
                compact_output(&output.stdout, &output.stderr)
            )));
        }
    }
    background_service_state()
}

#[tauri::command]
fn background_service_status() -> AppResult<BackgroundServiceStatus> {
    background_service_state()
}

#[tauri::command]
fn repair_background_service(state: State<'_, AppState>) -> AppResult<BackgroundServiceStatus> {
    install_background_service(&state.db_path)
}

fn due_job_ids(state: &AppState) -> AppResult<Vec<i64>> {
    let connection = connect(&state.db_path)?;
    let now = now_string();
    let mut statement = connection.prepare(
        "SELECT id FROM jobs
         WHERE enabled = 1 AND status != 'running'
               AND next_run_at IS NOT NULL AND next_run_at <= ?1
         ORDER BY next_run_at",
    )?;
    let ids = statement
        .query_map([now], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids)
}

async fn scheduler_loop(state: AppState) {
    tokio::time::sleep(StdDuration::from_secs(5)).await;
    loop {
        if let Ok(ids) = due_job_ids(&state) {
            for job_id in ids {
                let _ = execute_job(job_id, state.clone()).await;
            }
        }
        tokio::time::sleep(StdDuration::from_secs(30)).await;
    }
}

fn start_scheduler(state: AppState) {
    tauri::async_runtime::spawn(scheduler_loop(state));
}

fn run_service_inner(database_path: PathBuf) -> AppResult<()> {
    if let Some(parent) = database_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    initialize_database(&database_path)?;
    reset_interrupted_jobs(&database_path)?;
    let state = AppState {
        db_path: database_path,
        running_jobs: Arc::new(Mutex::new(HashSet::new())),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            AppError::Transfer(format!("Could not start the backup service: {error}"))
        })?;
    runtime.block_on(scheduler_loop(state));
    Ok(())
}

pub fn run_service(database_path: PathBuf) -> Result<(), String> {
    run_service_inner(database_path).map_err(|error| error.to_string())
}

fn tray_pixels() -> Vec<u8> {
    let mut pixels = Vec::with_capacity(16 * 16 * 4);
    for y in 0..16 {
        for x in 0..16 {
            let rounded = !(!(2..=13).contains(&x) && !(2..=13).contains(&y));
            let mark = (x == 5 || x == 10) && (4..=11).contains(&y);
            let (r, g, b, a) = if !rounded {
                (0, 0, 0, 0)
            } else if mark {
                (218, 170, 84, 255)
            } else {
                (35, 101, 70, 255)
            };
            pixels.extend_from_slice(&[r, g, b, a]);
        }
    }
    pixels
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let state = AppState {
                db_path: data_dir.join("cloudfolder.sqlite3"),
                running_jobs: Arc::new(Mutex::new(HashSet::new())),
            };
            initialize_database(&state.db_path)?;
            app.manage(state.clone());
            let service_active = if cfg!(debug_assertions) {
                false
            } else {
                install_background_service(&state.db_path)
                    .map(|status| status.active)
                    .unwrap_or(false)
            };
            if !service_active {
                reset_interrupted_jobs(&state.db_path)?;
                start_scheduler(state);
            }

            let show = MenuItem::with_id(app, "show", "Open CloudFolder", true, None::<&str>)?;
            let quit = MenuItem::with_id(
                app,
                "quit",
                "Close app (backups stay on)",
                true,
                None::<&str>,
            )?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            TrayIconBuilder::new()
                .tooltip("CloudFolder Sync")
                .icon(tauri::image::Image::new_owned(tray_pixels(), 16, 16))
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_jobs,
            get_app_settings,
            update_app_settings,
            scan_large_files,
            create_job,
            update_job,
            set_job_enabled,
            cancel_job,
            delete_job,
            run_job,
            list_version_snapshots,
            list_version_files,
            restore_version_file,
            job_history,
            job_activity,
            list_error_logs,
            list_remotes,
            list_providers,
            connect_provider,
            connect_provider_with_fields,
            disconnect_remote,
            list_cloud_folders,
            create_cloud_folder,
            open_rclone_config,
            open_support_page,
            background_service_status,
            repair_background_service,
            check_for_updates,
            download_update,
            open_release_page,
            quit_app
        ])
        .run(tauri::generate_context!())
        .expect("error while running CloudFolder Sync");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn rejects_relative_source_paths() {
        let input = NewJob {
            name: "Documents".into(),
            source_paths: vec!["Documents".into()],
            destination: "drive:Backups".into(),
            interval_minutes: 60,
            backup_mode: "incremental".into(),
            retention_count: 5,
            exclude_patterns: Vec::new(),
            size_filter_mode: default_filter_mode(),
            max_file_size_mib: None,
            extension_filter_mode: default_filter_mode(),
            excluded_extensions: Vec::new(),
        };
        assert!(validate_new_job(&input).is_err());
    }

    #[test]
    fn rejects_destinations_without_a_remote() {
        let input = NewJob {
            name: "Documents".into(),
            source_paths: vec![std::env::temp_dir().to_string_lossy().into_owned()],
            destination: "Backups".into(),
            interval_minutes: 60,
            backup_mode: "incremental".into(),
            retention_count: 5,
            exclude_patterns: Vec::new(),
            size_filter_mode: default_filter_mode(),
            max_file_size_mib: None,
            extension_filter_mode: default_filter_mode(),
            excluded_extensions: Vec::new(),
        };
        assert!(validate_new_job(&input).is_err());
    }

    #[test]
    fn mirroring_rejects_individual_files() {
        let file_path =
            std::env::temp_dir().join(format!("cloudfolder-mirror-file-{}", std::process::id()));
        std::fs::write(&file_path, "file").expect("create mirror file fixture");
        let input = NewJob {
            name: "Single file mirror".into(),
            source_paths: vec![file_path.to_string_lossy().into_owned()],
            destination: "drive:Backups".into(),
            interval_minutes: 60,
            backup_mode: "mirror".into(),
            retention_count: 5,
            exclude_patterns: Vec::new(),
            size_filter_mode: default_filter_mode(),
            max_file_size_mib: None,
            extension_filter_mode: default_filter_mode(),
            excluded_extensions: Vec::new(),
        };
        assert!(validate_new_job(&input).is_err());
        std::fs::remove_file(file_path).expect("clean up mirror file fixture");
    }

    #[test]
    fn empty_command_output_has_a_friendly_message() {
        assert_eq!(
            compact_output(b"", b""),
            "Backup completed; no files needed uploading"
        );
    }

    #[test]
    fn reads_percentages_from_rclone_stats() {
        assert_eq!(
            rclone_progress_percent(
                "2026/07/26 NOTICE: 1.027 MiB / 2 MiB, 51%, 1.027 MiB/s, ETA 0s"
            ),
            Some(51)
        );
        assert_eq!(
            rclone_progress_percent("2026/07/26 ERROR: Google Drive is offline"),
            None
        );
    }

    #[test]
    fn progress_labels_explain_multiple_sources() {
        assert_eq!(
            source_progress_message("Pictures", 2, 3),
            "Copying Pictures (2 of 3)"
        );
        assert_eq!(
            source_progress_message("Documents", 1, 1),
            "Copying Documents"
        );
    }

    #[test]
    fn activity_lines_are_cleaned_and_classified() {
        assert_eq!(
            clean_rclone_log_line(
                "2026/07/28 09:14:00 NOTICE: 1 MiB / 2 MiB, 50%, 1 MiB/s, ETA 1s"
            ),
            "1 MiB / 2 MiB, 50%, 1 MiB/s, ETA 1s"
        );
        assert_eq!(activity_state_for_line("Checking files", false), "scanning");
        assert_eq!(activity_state_for_line("Retry 1/3", false), "retrying");
        assert_eq!(
            activity_state_for_line("rate limit waiting", false),
            "waiting"
        );
        assert_eq!(activity_state_for_line("transfer failed", false), "error");
        assert_eq!(activity_state_for_line("50%", true), "copying");
    }

    #[test]
    fn activity_log_keeps_the_latest_five_hundred_entries() {
        let connection = Connection::open_in_memory().expect("open activity database");
        connection
            .execute_batch(
                "CREATE TABLE backup_activity (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    job_id INTEGER NOT NULL,
                    occurred_at TEXT NOT NULL,
                    state TEXT NOT NULL,
                    message TEXT NOT NULL
                );",
            )
            .expect("create activity table");
        for index in 0..505 {
            record_activity(&connection, 1, "info", &format!("Message {index}"))
                .expect("record activity");
        }
        let entries = query_job_activity(&connection, 1).expect("query activity");
        assert_eq!(entries.len(), 500);
        assert_eq!(entries.first().expect("first entry").message, "Message 5");
        assert_eq!(entries.last().expect("last entry").message, "Message 504");
    }

    #[test]
    fn cancel_request_is_seen_by_the_backup_worker() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cloudfolder-cancel-test-{}-{unique}",
            std::process::id()
        ));
        let source = root.join("source");
        let destination = root.join("destination");
        std::fs::create_dir_all(&source).expect("create cancellation source");
        std::fs::create_dir_all(&destination).expect("create cancellation destination");
        std::fs::write(source.join("should-not-copy.txt"), "cancel me")
            .expect("write cancellation fixture");
        let database_path = root.join("cloudfolder.sqlite3");
        initialize_database(&database_path).expect("initialize cancellation database");
        let connection = connect(&database_path).expect("open cancellation database");
        connection
            .execute(
                "INSERT INTO jobs
                    (id, name, source_path, destination, interval_minutes,
                     enabled, status, created_at)
                 VALUES (1, 'Cancelled backup', ?1, ?2, 60, 1, 'running', ?3)",
                params![
                    serde_json::to_string(&vec![source.to_string_lossy().into_owned()])
                        .expect("encode cancellation source"),
                    format!(":local:{}", destination.display()),
                    now_string()
                ],
            )
            .expect("insert running cancellation job");

        let message = request_job_cancellation(&connection, 1).expect("request cancellation");
        assert!(message.contains("Stopping"));
        let requested = connection
            .query_row(
                "SELECT cancel_requested FROM jobs WHERE id = 1",
                [],
                |row| row.get::<_, bool>(0),
            )
            .expect("read cancellation request");
        assert!(requested);
        let job = get_job(&connection, 1).expect("read cancellation job");
        drop(connection);

        let reporter = ProgressReporter {
            db_path: database_path,
            job_id: 1,
        };
        let result = perform_copy(&job, Some(&reporter));
        assert!(matches!(result, Err(AppError::Cancelled(_))));
        assert!(!destination.join("should-not-copy.txt").exists());

        std::fs::remove_dir_all(root).expect("clean up cancellation test");
    }

    #[test]
    fn retention_limits_are_validated() {
        let mut input = NewJob {
            name: "Documents".into(),
            source_paths: vec![std::env::temp_dir().to_string_lossy().into_owned()],
            destination: "drive:Backups".into(),
            interval_minutes: 60,
            backup_mode: "incremental".into(),
            retention_count: 5,
            exclude_patterns: Vec::new(),
            size_filter_mode: default_filter_mode(),
            max_file_size_mib: None,
            extension_filter_mode: default_filter_mode(),
            excluded_extensions: Vec::new(),
        };
        assert!(validate_new_job(&input).is_ok());
        input.retention_count = 0;
        assert!(validate_new_job(&input).is_ok());
        input.retention_count = 51;
        assert!(validate_new_job(&input).is_err());
        input.retention_count = 5;
        input.exclude_patterns = vec!["**/target/**".into(), "**/.git/**".into()];
        assert!(validate_new_job(&input).is_ok());
        input.exclude_patterns = vec!["**".into()];
        assert!(validate_new_job(&input).is_err());
        input.exclude_patterns = vec!["**/target/**".into(), "**/TARGET/**".into()];
        assert!(validate_new_job(&input).is_err());
    }

    #[test]
    fn previous_file_paths_are_scoped_and_traversal_safe() {
        let job = SyncJob {
            id: 42,
            name: "Renamable backup".into(),
            source_paths: vec!["/home/ryan/Documents".into()],
            destination: "CloudFolder:Backups/Documents".into(),
            interval_minutes: 60,
            backup_mode: "incremental".into(),
            last_full_at: None,
            enabled: true,
            last_run_at: None,
            next_run_at: None,
            status: "ready".into(),
            last_message: None,
            progress_percent: 0,
            progress_message: None,
            retention_count: 5,
            exclude_patterns: Vec::new(),
            size_filter_mode: default_filter_mode(),
            max_file_size_mib: None,
            extension_filter_mode: default_filter_mode(),
            excluded_extensions: Vec::new(),
            created_at: now_string(),
        };
        assert_eq!(
            version_history_root(&job).expect("build version root"),
            "CloudFolder:CloudFolder Previous Files/Backup 42"
        );
        assert!(validate_snapshot_name("2026-07-26_22-30-00").is_ok());
        assert!(validate_snapshot_name("../../Backups").is_err());
        assert!(validate_version_file_path("Documents/report.txt").is_ok());
        assert!(validate_version_file_path("../report.txt").is_err());
        assert!(validate_version_file_path("/home/ryan/report.txt").is_err());
    }

    #[test]
    fn restored_files_never_overwrite_an_existing_file() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cloudfolder-restore-name-test-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create restore test directory");
        std::fs::write(root.join("report.txt"), "current").expect("create current file");
        let restored_path = unique_restored_path(&root, "report.txt");
        assert_eq!(
            restored_path.file_name().and_then(|value| value.to_str()),
            Some("report (restored 1).txt")
        );
        std::fs::remove_dir_all(root).expect("clean up restore test");
    }

    #[test]
    fn retention_removes_only_snapshots_over_the_limit() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cloudfolder-retention-test-{}-{unique}",
            std::process::id()
        ));
        let destination = root.join("destination");
        std::fs::create_dir_all(&destination).expect("create retention destination");
        let job = SyncJob {
            id: 77,
            name: "Retention".into(),
            source_paths: vec![root.to_string_lossy().into_owned()],
            destination: format!(":local:{}", destination.display()),
            interval_minutes: 60,
            backup_mode: "incremental".into(),
            last_full_at: None,
            enabled: true,
            last_run_at: None,
            next_run_at: None,
            status: "ready".into(),
            last_message: None,
            progress_percent: 0,
            progress_message: None,
            retention_count: 2,
            exclude_patterns: Vec::new(),
            size_filter_mode: default_filter_mode(),
            max_file_size_mib: None,
            extension_filter_mode: default_filter_mode(),
            excluded_extensions: Vec::new(),
            created_at: now_string(),
        };
        let history_root = version_history_root(&job)
            .expect("build retention history path")
            .strip_prefix(":local:")
            .expect("local history path")
            .to_owned();
        assert!(version_snapshots_for_job(&job)
            .expect("missing history should be empty")
            .is_empty());
        for snapshot in [
            "2026-07-24_12-00-00",
            "2026-07-25_12-00-00",
            "2026-07-26_12-00-00",
        ] {
            let snapshot_dir = Path::new(&history_root).join(snapshot);
            std::fs::create_dir_all(&snapshot_dir).expect("create snapshot fixture");
            std::fs::write(snapshot_dir.join("old.txt"), snapshot).expect("write snapshot fixture");
        }

        prune_version_history(&job).expect("prune old snapshots");
        assert!(!Path::new(&history_root)
            .join("2026-07-24_12-00-00")
            .exists());
        assert!(Path::new(&history_root)
            .join("2026-07-25_12-00-00")
            .exists());
        assert!(Path::new(&history_root)
            .join("2026-07-26_12-00-00")
            .exists());

        std::fs::remove_dir_all(root).expect("clean up retention test");
    }

    #[test]
    fn error_log_only_returns_failed_runs() {
        let connection = Connection::open_in_memory().expect("open test database");
        connection
            .execute_batch(
                "
                CREATE TABLE jobs (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL
                );
                CREATE TABLE runs (
                    id INTEGER PRIMARY KEY,
                    job_id INTEGER NOT NULL,
                    started_at TEXT NOT NULL,
                    finished_at TEXT NOT NULL,
                    status TEXT NOT NULL,
                    message TEXT NOT NULL
                );
                INSERT INTO jobs (id, name) VALUES (1, 'Documents');
                INSERT INTO runs
                    (id, job_id, started_at, finished_at, status, message)
                VALUES
                    (1, 1, 'start-one', 'finish-one', 'success', 'All good'),
                    (2, 1, 'start-two', 'finish-two', 'error', 'Drive offline');
                ",
            )
            .expect("create error log fixtures");

        let logs = query_error_logs(&connection).expect("query error logs");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].job_name, "Documents");
        assert_eq!(logs[0].message, "Drive offline");
    }

    #[test]
    fn editing_a_job_keeps_its_run_history() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cloudfolder-edit-test-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create edit test directory");
        let database_path = root.join("cloudfolder.sqlite3");
        initialize_database(&database_path).expect("initialize test database");
        let connection = connect(&database_path).expect("open test database");
        connection
            .execute(
                "INSERT INTO jobs
                    (id, name, source_path, destination, interval_minutes,
                     enabled, next_run_at, status, created_at)
                 VALUES (1, 'Old name', ?1, 'CloudFolder:Old', 60,
                         1, ?2, 'ready', ?3)",
                params![
                    serde_json::to_string(&vec![root.to_string_lossy().into_owned()])
                        .expect("encode source"),
                    add_minutes(60),
                    now_string()
                ],
            )
            .expect("insert job");
        connection
            .execute(
                "INSERT INTO runs
                    (job_id, started_at, finished_at, status, message)
                 VALUES (1, 'start', 'finish', 'success', 'Done')",
                [],
            )
            .expect("insert run history");

        let input = NewJob {
            name: "Edited name".into(),
            source_paths: vec![root.to_string_lossy().into_owned()],
            destination: "CloudFolder:New".into(),
            interval_minutes: 180,
            backup_mode: "incremental".into(),
            retention_count: 7,
            exclude_patterns: vec!["target/**".into(), "**/target/**".into()],
            size_filter_mode: "custom".into(),
            max_file_size_mib: Some(250),
            extension_filter_mode: "custom".into(),
            excluded_extensions: vec!["iso".into()],
        };
        let updated =
            update_job_record(&connection, 1, &input).expect("update existing backup job");
        let run_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM runs WHERE job_id = 1", [], |row| {
                row.get(0)
            })
            .expect("count run history");

        assert_eq!(updated.name, "Edited name");
        assert_eq!(updated.destination, "CloudFolder:New");
        assert_eq!(updated.interval_minutes, 180);
        assert_eq!(updated.retention_count, 7);
        assert_eq!(updated.exclude_patterns, input.exclude_patterns);
        assert_eq!(updated.size_filter_mode, "custom");
        assert_eq!(updated.max_file_size_mib, Some(250));
        assert_eq!(updated.extension_filter_mode, "custom");
        assert_eq!(updated.excluded_extensions, vec!["iso"]);
        assert_eq!(run_count, 1);

        drop(connection);
        std::fs::remove_dir_all(root).expect("clean up edit test");
    }

    #[test]
    fn old_single_source_jobs_are_migrated_when_read() {
        assert_eq!(
            decode_source_paths("/home/ryan/Documents"),
            vec!["/home/ryan/Documents"]
        );
        assert_eq!(
            decode_source_paths(r#"["/home/ryan/Documents","/home/ryan/Pictures"]"#),
            vec!["/home/ryan/Documents", "/home/ryan/Pictures"]
        );
    }

    #[test]
    fn google_setup_requests_folder_browsing_access() {
        let drive = providers::catalog()
            .iter()
            .find(|provider| provider.id == "google_drive")
            .expect("Google Drive should stay in the catalog");
        assert_eq!(drive.backend, "drive");
        assert_eq!(providers::remote_name_for(drive, &[]), "CloudFolder");
        assert!(drive
            .stored_options
            .iter()
            .any(|(key, value)| *key == "scope" && *value == "drive"));
        assert!(drive
            .config_answers
            .iter()
            .any(|(key, value)| *key == "config_is_local" && *value == "true"));
    }

    #[test]
    fn cloud_folder_paths_are_joined_without_a_leading_slash() {
        assert_eq!(join_cloud_path("", "Photos"), "Photos");
        assert_eq!(
            join_cloud_path("Backups/2026", "Pictures"),
            "Backups/2026/Pictures"
        );
        assert_eq!(
            cloud_destination("CloudFolder:", "Backups/2026"),
            "CloudFolder:Backups/2026"
        );
    }

    #[test]
    fn cloud_folder_paths_reject_parent_segments() {
        assert!(clean_cloud_path("Backups/../Photos").is_err());
        assert!(clean_cloud_path("/Backups/Photos/").is_ok());
    }

    #[test]
    fn safe_copy_transfers_a_file_without_deleting_destination_content() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cloudfolder-copy-test-{}-{unique}",
            std::process::id()
        ));
        let source_dir = root.join("source");
        let destination_dir = root.join("destination");
        std::fs::create_dir_all(&source_dir).expect("create test source");
        std::fs::create_dir_all(&destination_dir).expect("create test destination");
        std::fs::write(source_dir.join("new.txt"), "new backup content")
            .expect("write source fixture");
        std::fs::create_dir_all(source_dir.join("project/target/debug"))
            .expect("create ignored build folder");
        std::fs::create_dir_all(source_dir.join("target/debug"))
            .expect("create ignored top-level build folder");
        std::fs::create_dir_all(source_dir.join("web/node_modules/package"))
            .expect("create ignored dependency folder");
        std::fs::create_dir_all(source_dir.join(".git/objects"))
            .expect("create ignored git folder");
        std::fs::write(
            source_dir.join("project/target/debug/generated.o"),
            "rebuildable artifact",
        )
        .expect("write ignored build artifact");
        std::fs::write(
            source_dir.join("target/debug/top-level.o"),
            "rebuildable top-level artifact",
        )
        .expect("write ignored top-level build artifact");
        std::fs::write(
            source_dir.join("web/node_modules/package/index.js"),
            "installed dependency",
        )
        .expect("write ignored dependency artifact");
        std::fs::write(source_dir.join(".git/objects/object"), "git object")
            .expect("write ignored git artifact");
        std::fs::write(destination_dir.join("existing.txt"), "must remain")
            .expect("write destination fixture");

        let job = SyncJob {
            id: 1,
            name: "Integration test".into(),
            source_paths: vec![source_dir.to_string_lossy().into_owned()],
            destination: format!(":local:{}", destination_dir.display()),
            interval_minutes: 60,
            backup_mode: "incremental".into(),
            last_full_at: None,
            enabled: true,
            last_run_at: None,
            next_run_at: None,
            status: "ready".into(),
            last_message: None,
            progress_percent: 0,
            progress_message: None,
            retention_count: 2,
            exclude_patterns: vec![
                "target/**".into(),
                "**/target/**".into(),
                "node_modules/**".into(),
                "**/node_modules/**".into(),
                ".git/**".into(),
                "**/.git/**".into(),
            ],
            size_filter_mode: default_filter_mode(),
            max_file_size_mib: None,
            extension_filter_mode: default_filter_mode(),
            excluded_extensions: Vec::new(),
            created_at: now_string(),
        };

        let result = perform_copy(&job, None);
        assert!(result.is_ok(), "copy failed: {result:?}");
        assert_eq!(
            std::fs::read_to_string(destination_dir.join("new.txt"))
                .expect("read transferred file"),
            "new backup content"
        );
        assert_eq!(
            std::fs::read_to_string(destination_dir.join("existing.txt"))
                .expect("read pre-existing file"),
            "must remain"
        );
        assert!(!destination_dir
            .join("project/target/debug/generated.o")
            .exists());
        assert!(!destination_dir.join("target/debug/top-level.o").exists());
        assert!(!destination_dir
            .join("web/node_modules/package/index.js")
            .exists());
        assert!(!destination_dir.join(".git/objects/object").exists());
        std::fs::write(source_dir.join("new.txt"), "updated backup content")
            .expect("update source fixture");
        perform_copy(&job, None).expect("perform update with safety copy");
        let history_root = version_history_root(&job)
            .expect("build incremental history path")
            .strip_prefix(":local:")
            .expect("local history path")
            .to_owned();
        let snapshots = std::fs::read_dir(history_root)
            .expect("read incremental safety copies")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect incremental safety copies");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(
            std::fs::read_to_string(snapshots[0].path().join("new.txt"))
                .expect("read retained previous content"),
            "new backup content"
        );
        let restore_dir = root.join("restored");
        std::fs::create_dir_all(&restore_dir).expect("create restore destination");
        std::fs::write(restore_dir.join("new.txt"), "current local copy")
            .expect("create restore name collision");
        let snapshot_name = snapshots[0].file_name().to_string_lossy().into_owned();
        let restored = restore_version_file_for_job(&job, &snapshot_name, "new.txt", &restore_dir)
            .expect("restore previous content");
        assert!(restored.path.ends_with("new (restored 1).txt"));
        assert_eq!(
            std::fs::read_to_string(&restored.path).expect("read restored previous content"),
            "new backup content"
        );

        std::fs::remove_dir_all(root).expect("clean up copy test");
    }

    #[test]
    fn multiple_sources_are_kept_in_separate_destination_folders() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cloudfolder-multi-test-{}-{unique}",
            std::process::id()
        ));
        let documents = root.join("Documents");
        let pictures = root.join("Pictures");
        let destination = root.join("destination");
        std::fs::create_dir_all(&documents).expect("create documents fixture");
        std::fs::create_dir_all(&pictures).expect("create pictures fixture");
        std::fs::create_dir_all(&destination).expect("create destination fixture");
        std::fs::write(documents.join("same-name.txt"), "document content")
            .expect("write document fixture");
        std::fs::write(pictures.join("same-name.txt"), "picture content")
            .expect("write picture fixture");

        let job = SyncJob {
            id: 2,
            name: "Multiple sources".into(),
            source_paths: vec![
                documents.to_string_lossy().into_owned(),
                pictures.to_string_lossy().into_owned(),
            ],
            destination: format!(":local:{}", destination.display()),
            interval_minutes: 60,
            backup_mode: "incremental".into(),
            last_full_at: None,
            enabled: true,
            last_run_at: None,
            next_run_at: None,
            status: "ready".into(),
            last_message: None,
            progress_percent: 0,
            progress_message: None,
            retention_count: 0,
            exclude_patterns: Vec::new(),
            size_filter_mode: default_filter_mode(),
            max_file_size_mib: None,
            extension_filter_mode: default_filter_mode(),
            excluded_extensions: Vec::new(),
            created_at: now_string(),
        };

        let result = perform_copy(&job, None);
        assert!(result.is_ok(), "multi-source copy failed: {result:?}");
        assert_eq!(
            std::fs::read_to_string(destination.join("Documents/same-name.txt"))
                .expect("read backed up document"),
            "document content"
        );
        assert_eq!(
            std::fs::read_to_string(destination.join("Pictures/same-name.txt"))
                .expect("read backed up picture"),
            "picture content"
        );

        std::fs::remove_dir_all(root).expect("clean up multi-source test");
    }

    #[test]
    fn full_backup_creates_a_dated_snapshot() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cloudfolder-full-test-{}-{unique}",
            std::process::id()
        ));
        let source = root.join("source");
        let destination = root.join("destination");
        std::fs::create_dir_all(&source).expect("create full source");
        std::fs::create_dir_all(&destination).expect("create full destination");
        std::fs::write(source.join("photo.jpg"), "snapshot content").expect("write full fixture");
        let job = SyncJob {
            id: 3,
            name: "Full snapshot".into(),
            source_paths: vec![source.to_string_lossy().into_owned()],
            destination: format!(":local:{}", destination.display()),
            interval_minutes: 1440,
            backup_mode: "full".into(),
            last_full_at: None,
            enabled: true,
            last_run_at: None,
            next_run_at: None,
            status: "ready".into(),
            last_message: None,
            progress_percent: 0,
            progress_message: None,
            retention_count: 0,
            exclude_patterns: Vec::new(),
            size_filter_mode: default_filter_mode(),
            max_file_size_mib: None,
            extension_filter_mode: default_filter_mode(),
            excluded_extensions: Vec::new(),
            created_at: now_string(),
        };

        let outcome = perform_copy(&job, None).expect("perform full backup");
        let snapshots = std::fs::read_dir(destination.join("Full"))
            .expect("read dated snapshots")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect dated snapshots");
        assert!(outcome.full_completed);
        assert_eq!(snapshots.len(), 1);
        assert!(snapshots[0].path().join("photo.jpg").exists());

        std::fs::remove_dir_all(root).expect("clean up full test");
    }

    #[test]
    fn mirroring_removes_files_missing_from_the_source() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cloudfolder-mirror-test-{}-{unique}",
            std::process::id()
        ));
        let source = root.join("source");
        let destination = root.join("destination");
        std::fs::create_dir_all(&source).expect("create mirror source");
        std::fs::create_dir_all(&destination).expect("create mirror destination");
        std::fs::create_dir_all(destination.join(".git/objects"))
            .expect("create excluded mirror destination");
        std::fs::write(source.join("keep.txt"), "keep").expect("write mirror source");
        std::fs::write(destination.join("remove.txt"), "remove").expect("write extra mirror file");
        std::fs::write(destination.join(".git/objects/keep"), "ignored cloud file")
            .expect("write excluded mirror file");
        let job = SyncJob {
            id: 4,
            name: "Mirror".into(),
            source_paths: vec![source.to_string_lossy().into_owned()],
            destination: format!(":local:{}", destination.display()),
            interval_minutes: 60,
            backup_mode: "mirror".into(),
            last_full_at: None,
            enabled: true,
            last_run_at: None,
            next_run_at: None,
            status: "ready".into(),
            last_message: None,
            progress_percent: 0,
            progress_message: None,
            retention_count: 2,
            exclude_patterns: vec![".git/**".into(), "**/.git/**".into()],
            size_filter_mode: default_filter_mode(),
            max_file_size_mib: None,
            extension_filter_mode: default_filter_mode(),
            excluded_extensions: Vec::new(),
            created_at: now_string(),
        };

        let history_root = version_history_root(&job)
            .expect("build mirror history path")
            .strip_prefix(":local:")
            .expect("local history path")
            .to_owned();
        perform_copy(&job, None).expect("perform mirror");
        assert!(destination.join("keep.txt").exists());
        assert!(!destination.join("remove.txt").exists());
        assert!(destination.join(".git/objects/keep").exists());
        let snapshots = std::fs::read_dir(history_root)
            .expect("read mirror safety copies")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect mirror safety copies");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(
            std::fs::read_to_string(snapshots[0].path().join("remove.txt"))
                .expect("read retained deleted file"),
            "remove"
        );

        std::fs::remove_dir_all(root).expect("clean up mirror test");
    }

    #[test]
    fn systemd_unit_uses_headless_mode_and_quotes_paths() {
        let unit = background_service_unit(
            Path::new("/opt/CloudFolder Sync/cloudfolder-sync"),
            Path::new("/home/ryan/My Data/cloudfolder.sqlite3"),
        );
        assert!(unit.contains("ExecStart=\"/opt/CloudFolder Sync/cloudfolder-sync\" --service"));
        assert!(unit.contains("--database \"/home/ryan/My Data/cloudfolder.sqlite3\""));
        assert!(unit.contains(&format!(
            "Environment=CLOUDFOLDER_VERSION={}",
            env!("CARGO_PKG_VERSION")
        )));
        assert!(unit.contains("Restart=always"));
    }

    #[test]
    fn old_databases_gain_backup_mode_columns() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cloudfolder-migration-test-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create migration directory");
        let database_path = root.join("old.sqlite3");
        let connection = connect(&database_path).expect("open old database");
        connection
            .execute_batch(
                "CREATE TABLE jobs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL,
                    source_path TEXT NOT NULL,
                    destination TEXT NOT NULL,
                    interval_minutes INTEGER NOT NULL,
                    enabled INTEGER NOT NULL DEFAULT 1,
                    last_run_at TEXT,
                    next_run_at TEXT,
                    status TEXT NOT NULL DEFAULT 'ready',
                    last_message TEXT,
                    created_at TEXT NOT NULL
                );",
            )
            .expect("create old jobs table");
        drop(connection);

        initialize_database(&database_path).expect("migrate old database");
        let connection = connect(&database_path).expect("open migrated database");
        let mut statement = connection
            .prepare("PRAGMA table_info(jobs)")
            .expect("inspect migrated columns");
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query migrated columns")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect migrated columns");
        assert!(columns.contains(&"backup_mode".to_string()));
        assert!(columns.contains(&"last_full_at".to_string()));
        assert!(columns.contains(&"progress_percent".to_string()));
        assert!(columns.contains(&"progress_message".to_string()));
        assert!(columns.contains(&"retention_count".to_string()));
        assert!(columns.contains(&"exclude_patterns".to_string()));
        assert!(columns.contains(&"cancel_requested".to_string()));
        assert!(columns.contains(&"size_filter_mode".to_string()));
        assert!(columns.contains(&"max_file_size_mib".to_string()));
        assert!(columns.contains(&"extension_filter_mode".to_string()));
        assert!(columns.contains(&"excluded_extensions".to_string()));
        let activity_table_exists = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'table' AND name = 'backup_activity'
                )",
                [],
                |row| row.get::<_, bool>(0),
            )
            .expect("check activity table migration");
        assert!(activity_table_exists);
        let settings = load_app_settings(&connection).expect("load migrated app settings");
        assert_eq!(settings.max_file_size_mib, None);
        assert!(settings.excluded_extensions.is_empty());

        drop(statement);
        drop(connection);
        std::fs::remove_dir_all(root).expect("clean up migration test");
    }

    #[test]
    fn differential_backup_creates_a_baseline_then_dated_changes() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cloudfolder-differential-test-{}-{unique}",
            std::process::id()
        ));
        let source = root.join("source");
        let destination = root.join("destination");
        std::fs::create_dir_all(&source).expect("create differential source");
        std::fs::create_dir_all(&destination).expect("create differential destination");
        std::fs::write(source.join("original.txt"), "baseline").expect("write baseline fixture");
        let mut job = SyncJob {
            id: 5,
            name: "Differential".into(),
            source_paths: vec![source.to_string_lossy().into_owned()],
            destination: format!(":local:{}", destination.display()),
            interval_minutes: 60,
            backup_mode: "differential".into(),
            last_full_at: None,
            enabled: true,
            last_run_at: None,
            next_run_at: None,
            status: "ready".into(),
            last_message: None,
            progress_percent: 0,
            progress_message: None,
            retention_count: 0,
            exclude_patterns: Vec::new(),
            size_filter_mode: default_filter_mode(),
            max_file_size_mib: None,
            extension_filter_mode: default_filter_mode(),
            excluded_extensions: Vec::new(),
            created_at: now_string(),
        };

        let baseline = perform_copy(&job, None).expect("create differential baseline");
        assert!(baseline.full_completed);
        assert!(destination.join("Baseline").exists());

        job.last_full_at = Some((Utc::now() - Duration::minutes(1)).to_rfc3339());
        std::fs::write(source.join("changed.txt"), "changed").expect("write differential change");
        let difference = perform_copy(&job, None).expect("create differential change set");
        assert!(!difference.full_completed);
        let changes = std::fs::read_dir(destination.join("Differential"))
            .expect("read differential folders")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect differential folders");
        assert_eq!(changes.len(), 1);
        assert!(changes[0].path().join("changed.txt").exists());

        std::fs::remove_dir_all(root).expect("clean up differential test");
    }

    #[test]
    fn updater_selects_the_matching_linux_package() {
        let release = GitHubRelease {
            tag_name: "v0.6.0".into(),
            name: Some("CloudFolder 0.6.0".into()),
            body: None,
            html_url: "https://github.com/Ryanrobertolson/cloudfolder-sync/releases/tag/v0.6.0"
                .into(),
            published_at: None,
            assets: vec![
                GitHubAsset {
                    name: "CloudFolder Sync_0.6.0_amd64.deb".into(),
                    browser_download_url: "https://github.com/deb".into(),
                    size: 10,
                },
                GitHubAsset {
                    name: "CloudFolder Sync_0.6.0_amd64.AppImage".into(),
                    browser_download_url: "https://github.com/appimage".into(),
                    size: 20,
                },
            ],
        };
        assert!(release_asset(&release, "deb")
            .expect("find deb")
            .name
            .ends_with("_amd64.deb"));
        assert!(release_asset(&release, "appimage")
            .expect("find appimage")
            .name
            .ends_with(".AppImage"));
    }

    #[test]
    fn updater_rejects_non_github_and_unsafe_downloads() {
        assert!(validate_update_download(
            "https://github.com/Ryanrobertolson/cloudfolder-sync/releases/download/v0.6.0/app.deb",
            "CloudFolder Sync_0.6.0_amd64.deb"
        )
        .is_ok());
        assert!(validate_update_download(
            "https://example.com/fake.deb",
            "CloudFolder Sync_0.6.0_amd64.deb"
        )
        .is_err());
        assert!(validate_update_download(
            "https://github.com/Ryanrobertolson/cloudfolder-sync/releases/download/v0.6.0/app.deb",
            "../unsafe_amd64.deb"
        )
        .is_err());
    }

    #[test]
    fn filter_configuration_normalizes_and_validates_extensions() {
        assert_eq!(
            clean_extensions(&[".PDF".into(), " zip ".into()]).expect("valid extensions"),
            vec!["pdf", "zip"]
        );
        assert!(clean_extensions(&["tar.gz".into()]).is_err());
        assert!(clean_extensions(&["..zip".into()]).is_err());
        assert!(clean_extensions(&["pdf".into(), ".PDF".into()]).is_err());
        assert!(validate_max_file_size(Some(0)).is_err());
        assert!(validate_max_file_size(Some(100)).is_ok());
    }

    #[test]
    fn filter_configuration_resolves_inherit_custom_and_disabled_modes() {
        let settings = AppSettings {
            max_file_size_mib: Some(100),
            excluded_extensions: vec!["zip".into()],
        };

        assert_eq!(
            resolve_filter_values("inherit", None, "inherit", &[], &settings).expect("inherit"),
            EffectiveFilters {
                max_file_size_mib: Some(100),
                excluded_extensions: vec!["zip".into()],
            }
        );
        assert_eq!(
            resolve_filter_values("custom", Some(50), "custom", &["iso".into()], &settings)
                .expect("custom"),
            EffectiveFilters {
                max_file_size_mib: Some(50),
                excluded_extensions: vec!["iso".into()],
            }
        );
        assert_eq!(
            resolve_filter_values("disabled", Some(50), "disabled", &["iso".into()], &settings)
                .expect("disabled"),
            EffectiveFilters {
                max_file_size_mib: None,
                excluded_extensions: Vec::new(),
            }
        );
        assert!(resolve_filter_values("unknown", None, "inherit", &[], &settings).is_err());
    }

    #[test]
    fn large_file_scan_counts_only_files_over_threshold_without_following_symlinks() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cloudfolder-large-scan-{}-{unique}",
            std::process::id()
        ));
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).expect("create scan fixture");
        let threshold = 25 * 1024 * 1024;
        std::fs::File::create(root.join("exact.bin"))
            .expect("create exact file")
            .set_len(threshold)
            .expect("size exact file");
        std::fs::File::create(root.join("large.bin"))
            .expect("create large file")
            .set_len(threshold + 1)
            .expect("size large file");
        std::fs::File::create(nested.join("largest.iso"))
            .expect("create largest file")
            .set_len(threshold + 1024)
            .expect("size largest file");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&nested, root.join("nested-link"))
            .expect("create directory symlink");

        let result = scan_paths_for_large_files(
            &[
                root.to_string_lossy().into_owned(),
                nested.to_string_lossy().into_owned(),
            ],
            threshold,
        )
        .expect("scan source paths");

        assert_eq!(result.threshold_bytes, threshold);
        assert_eq!(result.matching_file_count, 2);
        assert_eq!(result.examples.len(), 2);
        assert!(result.examples[0].size_bytes > result.examples[1].size_bytes);
        assert!(result.examples[0].path.ends_with("largest.iso"));
        assert_eq!(result.unreadable_path_count, 0);

        std::fs::remove_dir_all(root).expect("clean up scan fixture");
    }

    #[test]
    fn rclone_filter_arguments_include_size_extensions_and_existing_rules() {
        let mut command = Command::new("rclone");
        append_filter_arguments(
            &mut command,
            &EffectiveFilters {
                max_file_size_mib: Some(100),
                excluded_extensions: vec!["zip".into(), "pdf".into()],
            },
            &["**/.git/**".into()],
        );
        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--max-size", "100M"]));
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--exclude", "*.[zZ][iI][pP]"]));
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--exclude", "*.[pP][dD][fF]"]));
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--exclude", "**/.git/**"]));
        assert!(!arguments.iter().any(|argument| argument == "--ignore-case"));
    }

    #[test]
    fn global_filter_update_invalidates_only_inheriting_baselines() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cloudfolder-filter-settings-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create settings fixture");
        let database_path = root.join("settings.sqlite3");
        initialize_database(&database_path).expect("initialize settings database");
        let mut connection = connect(&database_path).expect("open settings database");
        connection
            .execute(
                "INSERT INTO jobs
                    (id, name, source_path, destination, interval_minutes, last_full_at,
                     size_filter_mode, extension_filter_mode, created_at)
                 VALUES
                    (1, 'Inherited', '/tmp/source-one', 'drive:one', 60, 'baseline',
                     'inherit', 'inherit', 'now'),
                    (2, 'Disabled', '/tmp/source-two', 'drive:two', 60, 'baseline',
                     'disabled', 'disabled', 'now')",
                [],
            )
            .expect("insert settings jobs");

        update_app_settings_record(
            &mut connection,
            AppSettings {
                max_file_size_mib: Some(100),
                excluded_extensions: vec!["iso".into()],
            },
        )
        .expect("update app settings");

        let inherited_baseline: Option<String> = connection
            .query_row("SELECT last_full_at FROM jobs WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("read inherited baseline");
        let disabled_baseline: Option<String> = connection
            .query_row("SELECT last_full_at FROM jobs WHERE id = 2", [], |row| {
                row.get(0)
            })
            .expect("read disabled baseline");
        assert_eq!(inherited_baseline, None);
        assert_eq!(disabled_baseline.as_deref(), Some("baseline"));

        drop(connection);
        std::fs::remove_dir_all(root).expect("clean up settings fixture");
    }

    #[test]
    fn global_filter_update_is_rejected_while_a_backup_is_running() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cloudfolder-running-settings-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create running settings fixture");
        let database_path = root.join("settings.sqlite3");
        initialize_database(&database_path).expect("initialize settings database");
        let mut connection = connect(&database_path).expect("open settings database");
        connection
            .execute(
                "INSERT INTO jobs
                    (name, source_path, destination, interval_minutes, status, created_at)
                 VALUES ('Running', '/tmp/source', 'drive:backup', 60, 'running', 'now')",
                [],
            )
            .expect("insert running job");

        let result = update_app_settings_record(
            &mut connection,
            AppSettings {
                max_file_size_mib: Some(100),
                excluded_extensions: vec!["iso".into()],
            },
        );
        assert!(matches!(result, Err(AppError::Validation(_))));

        drop(connection);
        std::fs::remove_dir_all(root).expect("clean up running settings fixture");
    }

    #[test]
    fn exclude_patterns_match_folders_extensions_and_globs() {
        assert!(matches_exclude_pattern("target/debug/app", "target/**", false));
        assert!(matches_exclude_pattern("src/target/file.txt", "**/target/**", false));
        assert!(matches_exclude_pattern("dune/pfx/drive_c/windows", "**/pfx/**", true));
        assert!(matches_exclude_pattern("app.pyc", "*.pyc", false));
        assert!(matches_exclude_pattern("node_modules", "node_modules/**", true));
        assert!(!matches_exclude_pattern("src/main.rs", "target/**", false));
    }

    #[test]
    fn local_metadata_cache_detects_deltas_and_skips_unchanged() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("cloudfolder-cache-test-{}-{unique}", std::process::id()));
        let source = root.join("source");
        std::fs::create_dir_all(&source).expect("create source dir");
        let file1 = source.join("file1.txt");
        let file2 = source.join("file2.txt");
        std::fs::write(&file1, b"hello 1").expect("write file1");
        std::fs::write(&file2, b"hello 2").expect("write file2");

        let db_path = root.join("test.sqlite3");
        initialize_database(&db_path).expect("init db");
        let connection = connect(&db_path).expect("open test database");
        connection
            .execute(
                "INSERT INTO jobs (id, name, source_path, destination, interval_minutes, created_at)
                 VALUES (1, 'Test', '/tmp/source', 'drive:backup', 60, 'now')",
                [],
            )
            .expect("insert test job");
        drop(connection);

        let filters = EffectiveFilters {
            max_file_size_mib: None,
            excluded_extensions: Vec::new(),
        };

        // 1. First scan on empty cache
        let scan1 = scan_source_with_cache(&source, 1, 0, Some(&db_path), &filters, &[]).expect("scan1");
        assert_eq!(scan1.has_cache, false);
        assert_eq!(scan1.changed_files.len(), 2);
        assert_eq!(scan1.scanned_entries.len(), 2);

        // Commit cache
        commit_file_cache(&db_path, 1, 0, &scan1.scanned_entries).expect("commit");

        // 2. Second scan on unmodified files -> 0 changed files
        let scan2 = scan_source_with_cache(&source, 1, 0, Some(&db_path), &filters, &[]).expect("scan2");
        assert_eq!(scan2.has_cache, true);
        assert_eq!(scan2.changed_files.len(), 0);

        // 3. Modify one file -> exactly 1 changed file
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&file1, b"hello modified").expect("modify file1");

        let scan3 = scan_source_with_cache(&source, 1, 0, Some(&db_path), &filters, &[]).expect("scan3");
        assert_eq!(scan3.has_cache, true);
        assert_eq!(scan3.changed_files, vec!["file1.txt".to_string()]);

        std::fs::remove_dir_all(root).expect("clean up");
    }
}
