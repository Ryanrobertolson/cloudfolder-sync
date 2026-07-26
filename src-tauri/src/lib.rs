use chrono::{Duration, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::Command,
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

#[derive(Debug, Clone, Serialize)]
struct SyncJob {
    id: i64,
    name: String,
    source_paths: Vec<String>,
    destination: String,
    interval_minutes: i64,
    enabled: bool,
    last_run_at: Option<String>,
    next_run_at: Option<String>,
    status: String,
    last_message: Option<String>,
    created_at: String,
}

#[derive(Debug, Deserialize)]
struct NewJob {
    name: String,
    source_paths: Vec<String>,
    destination: String,
    interval_minutes: i64,
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
struct CloudFolderEntry {
    name: String,
    path: String,
}

#[derive(Debug, Deserialize)]
struct RcloneListItem {
    #[serde(rename = "Name")]
    name: String,
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
            enabled          INTEGER NOT NULL DEFAULT 1,
            last_run_at      TEXT,
            next_run_at      TEXT,
            status           TEXT NOT NULL DEFAULT 'ready',
            last_message     TEXT,
            created_at       TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS runs (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            job_id      INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
            started_at  TEXT NOT NULL,
            finished_at TEXT NOT NULL,
            status      TEXT NOT NULL,
            message     TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_jobs_due
            ON jobs(enabled, next_run_at);
        CREATE INDEX IF NOT EXISTS idx_runs_job
            ON runs(job_id, id DESC);
        ",
    )?;
    connection.execute(
        "UPDATE jobs SET status = 'ready', last_message = 'Previous run was interrupted'
         WHERE status = 'running'",
        [],
    )?;
    Ok(())
}

fn map_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<SyncJob> {
    let stored_source: String = row.get(2)?;
    Ok(SyncJob {
        id: row.get(0)?,
        name: row.get(1)?,
        source_paths: decode_source_paths(&stored_source),
        destination: row.get(3)?,
        interval_minutes: row.get(4)?,
        enabled: row.get::<_, i64>(5)? != 0,
        last_run_at: row.get(6)?,
        next_run_at: row.get(7)?,
        status: row.get(8)?,
        last_message: row.get(9)?,
        created_at: row.get(10)?,
    })
}

fn decode_source_paths(stored: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(stored)
        .ok()
        .filter(|paths| !paths.is_empty())
        .unwrap_or_else(|| vec![stored.to_owned()])
}

fn get_job(connection: &Connection, job_id: i64) -> AppResult<SyncJob> {
    connection
        .query_row(
            "SELECT id, name, source_path, destination, interval_minutes, enabled,
                    last_run_at, next_run_at, status, last_message, created_at
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
        "SELECT id, name, source_path, destination, interval_minutes, enabled,
                last_run_at, next_run_at, status, last_message, created_at
         FROM jobs ORDER BY created_at DESC",
    )?;
    let jobs = statement
        .query_map([], map_job)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(jobs)
}

#[tauri::command]
fn create_job(input: NewJob, state: State<'_, AppState>) -> AppResult<SyncJob> {
    validate_new_job(&input)?;
    let connection = connect(&state.db_path)?;
    let created_at = now_string();
    let next_run = add_minutes(input.interval_minutes);
    let stored_sources = serde_json::to_string(&input.source_paths)
        .map_err(|error| AppError::Validation(format!("Could not save the sources: {error}")))?;
    connection.execute(
        "INSERT INTO jobs
            (name, source_path, destination, interval_minutes, enabled,
             next_run_at, status, created_at)
         VALUES (?1, ?2, ?3, ?4, 1, ?5, 'ready', ?6)",
        params![
            input.name.trim(),
            stored_sources,
            input.destination,
            input.interval_minutes,
            next_run,
            created_at
        ],
    )?;
    get_job(&connection, connection.last_insert_rowid())
}

#[tauri::command]
fn set_job_enabled(job_id: i64, enabled: bool, state: State<'_, AppState>) -> AppResult<SyncJob> {
    let connection = connect(&state.db_path)?;
    let job = get_job(&connection, job_id)?;
    let next_run = if enabled {
        Some(add_minutes(job.interval_minutes))
    } else {
        None
    };
    connection.execute(
        "UPDATE jobs
         SET enabled = ?1, next_run_at = ?2, status = ?3
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

fn perform_copy(job: &SyncJob) -> AppResult<String> {
    let multiple_sources = job.source_paths.len() > 1;
    let mut completed = Vec::new();
    let mut failures = Vec::new();

    for source_path in &job.source_paths {
        let source = Path::new(source_path);
        let source_name = source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| source_path.clone());
        if !source.exists() {
            failures.push(format!(
                "{source_name}: source is unavailable; reconnect its drive and try again"
            ));
            continue;
        }
        let destination = if multiple_sources {
            child_cloud_destination(&job.destination, &source_name)
        } else {
            job.destination.clone()
        };
        let output = Command::new("rclone")
            .arg("copy")
            .arg(source_path)
            .arg(destination)
            .arg("--create-empty-src-dirs")
            .arg("--stats-one-line")
            .arg("--stats=10s")
            .arg("--retries=3")
            .output()
            .map_err(|error| {
                AppError::Transfer(format!(
                    "Could not start rclone: {error}. Install rclone and try again."
                ))
            })?;
        let message = compact_output(&output.stdout, &output.stderr);
        if output.status.success() {
            completed.push(format!("{source_name}: {message}"));
        } else {
            failures.push(format!("{source_name}: {message}"));
        }
    }

    if failures.is_empty() {
        Ok(completed.join("\n"))
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

fn child_cloud_destination(destination: &str, child_name: &str) -> String {
    if destination.ends_with(':') || destination.ends_with('/') {
        format!("{destination}{child_name}")
    } else {
        format!("{destination}/{child_name}")
    }
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
        let connection = connect(&state.db_path)?;
        let job = get_job(&connection, job_id)?;
        let started_at = now_string();
        connection.execute(
            "UPDATE jobs SET status = 'running', last_message = NULL WHERE id = ?1",
            [job_id],
        )?;
        drop(connection);

        let job_for_copy = job.clone();
        let copy_result = tauri::async_runtime::spawn_blocking(move || perform_copy(&job_for_copy))
            .await
            .map_err(|error| AppError::Transfer(format!("Backup worker failed: {error}")))?;

        let finished_at = now_string();
        let (status, message) = match copy_result {
            Ok(message) => ("success", message),
            Err(error) => ("error", error.to_string()),
        };
        let connection = connect(&state.db_path)?;
        connection.execute(
            "UPDATE jobs
             SET status = ?1, last_message = ?2, last_run_at = ?3, next_run_at = ?4
             WHERE id = ?5",
            params![
                status,
                message,
                finished_at,
                add_minutes(job.interval_minutes),
                job_id
            ],
        )?;
        connection.execute(
            "INSERT INTO runs
                (job_id, started_at, finished_at, status, message)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![job_id, started_at, finished_at, status, message],
        )?;
        Ok(RunRecord {
            id: connection.last_insert_rowid(),
            job_id,
            started_at,
            finished_at,
            status: status.into(),
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

#[tauri::command]
fn configured_remotes() -> AppResult<Vec<String>> {
    let output = Command::new("rclone")
        .arg("listremotes")
        .output()
        .map_err(|error| {
            AppError::Transfer(format!(
                "Could not run rclone: {error}. Install rclone and try again."
            ))
        })?;
    if !output.status.success() {
        return Err(AppError::Transfer(compact_output(
            &output.stdout,
            &output.stderr,
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

#[tauri::command]
fn list_remotes() -> AppResult<Vec<String>> {
    configured_remotes()
}

const GOOGLE_REMOTE_NAME: &str = "CloudFolder:";

fn google_drive_config_args() -> [&'static str; 8] {
    [
        "config",
        "create",
        "CloudFolder",
        "drive",
        "scope",
        "drive",
        "config_is_local",
        "true",
    ]
}

fn google_drive_update_args() -> [&'static str; 8] {
    [
        "config",
        "update",
        "CloudFolder",
        "scope",
        "drive",
        "config_is_local",
        "true",
        "--auto-confirm",
    ]
}

fn cloudfolder_drive_has_browse_access() -> AppResult<bool> {
    let output = Command::new("rclone")
        .args(["config", "redacted", "CloudFolder"])
        .output()?;
    if !output.status.success() {
        return Err(AppError::Transfer(compact_output(
            &output.stdout,
            &output.stderr,
        )));
    }
    let config = String::from_utf8_lossy(&output.stdout);
    let is_drive = config
        .lines()
        .any(|line| line.trim().eq_ignore_ascii_case("type = drive"));
    if !is_drive {
        return Err(AppError::Validation(
            "A different cloud connection is already named CloudFolder. Rename it in advanced setup and try again."
                .into(),
        ));
    }
    Ok(config
        .lines()
        .any(|line| line.trim().eq_ignore_ascii_case("scope = drive")))
}

#[tauri::command]
async fn connect_google_drive() -> AppResult<String> {
    tauri::async_runtime::spawn_blocking(|| {
        let already_exists = configured_remotes()?
            .iter()
            .any(|remote| remote == GOOGLE_REMOTE_NAME);
        if already_exists && cloudfolder_drive_has_browse_access()? {
            return Ok(GOOGLE_REMOTE_NAME.to_owned());
        }

        let mut command = Command::new("rclone");
        if already_exists {
            command.args(google_drive_update_args());
        } else {
            command.args(google_drive_config_args());
        }
        let output = command.output().map_err(|error| {
            AppError::Transfer(format!(
                "CloudFolder could not start Google sign-in: {error}"
            ))
        })?;

        if !output.status.success() {
            let details = compact_output(&output.stdout, &output.stderr);
            return Err(AppError::Transfer(format!(
                "Google Drive did not finish connecting. {details}"
            )));
        }

        if configured_remotes()?
            .iter()
            .any(|remote| remote == GOOGLE_REMOTE_NAME)
        {
            Ok(GOOGLE_REMOTE_NAME.to_owned())
        } else {
            Err(AppError::Transfer(
                "Google Drive sign-in finished, but the connection was not saved. Try again."
                    .into(),
            ))
        }
    })
    .await
    .map_err(|error| AppError::Transfer(format!("Google sign-in stopped: {error}")))?
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
        .arg("--no-modtime")
        .arg("--no-mimetype")
        .output()
        .map_err(|error| {
            AppError::Transfer(format!(
                "CloudFolder could not look inside Google Drive: {error}"
            ))
        })?;
    if !output.status.success() {
        return Err(AppError::Transfer(format!(
            "Google Drive could not open this folder. {}",
            compact_output(&output.stdout, &output.stderr)
        )));
    }

    let mut folders: Vec<CloudFolderEntry> =
        serde_json::from_slice::<Vec<RcloneListItem>>(&output.stdout)
            .map_err(|error| {
                AppError::Transfer(format!(
                    "Google Drive returned a folder list CloudFolder could not read: {error}"
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
                "CloudFolder could not create that Google Drive folder: {error}"
            ))
        })?;
    if !output.status.success() {
        return Err(AppError::Transfer(format!(
            "Google Drive could not create that folder. {}",
            compact_output(&output.stdout, &output.stderr)
        )));
    }
    Ok(new_path)
}

fn ensure_configured_remote(remote: &str) -> AppResult<()> {
    if !remote.ends_with(':')
        || !configured_remotes()?
            .iter()
            .any(|configured| configured == remote)
    {
        return Err(AppError::Validation(
            "Choose a connected Google Drive account first".into(),
        ));
    }
    Ok(())
}

fn clean_cloud_path(path: &str) -> AppResult<String> {
    let clean = path.trim_matches('/').trim().to_owned();
    if clean.split('/').any(|part| part == "." || part == "..") {
        return Err(AppError::Validation(
            "That Google Drive folder path is not valid".into(),
        ));
    }
    if clean.chars().any(char::is_control) {
        return Err(AppError::Validation(
            "That Google Drive folder path contains unsupported characters".into(),
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

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

fn due_job_ids(state: &AppState) -> AppResult<Vec<i64>> {
    let connection = connect(&state.db_path)?;
    let now = now_string();
    let mut statement = connection.prepare(
        "SELECT id FROM jobs
         WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ?1
         ORDER BY next_run_at",
    )?;
    let ids = statement
        .query_map([now], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids)
}

fn start_scheduler(state: AppState) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(StdDuration::from_secs(5)).await;
        loop {
            if let Ok(ids) = due_job_ids(&state) {
                for job_id in ids {
                    let _ = execute_job(job_id, state.clone()).await;
                }
            }
            tokio::time::sleep(StdDuration::from_secs(30)).await;
        }
    });
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
            start_scheduler(state);

            let show = MenuItem::with_id(app, "show", "Open CloudFolder", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
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
            create_job,
            set_job_enabled,
            delete_job,
            run_job,
            job_history,
            list_remotes,
            connect_google_drive,
            list_cloud_folders,
            create_cloud_folder,
            open_rclone_config,
            open_support_page,
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
        };
        assert!(validate_new_job(&input).is_err());
    }

    #[test]
    fn empty_command_output_has_a_friendly_message() {
        assert_eq!(
            compact_output(b"", b""),
            "Backup completed; no files needed uploading"
        );
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
        let arguments = google_drive_config_args();
        assert_eq!(arguments[2], "CloudFolder");
        assert_eq!(arguments[3], "drive");
        assert_eq!(arguments[5], "drive");
        assert_eq!(arguments[7], "true");
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
        std::fs::write(destination_dir.join("existing.txt"), "must remain")
            .expect("write destination fixture");

        let job = SyncJob {
            id: 1,
            name: "Integration test".into(),
            source_paths: vec![source_dir.to_string_lossy().into_owned()],
            destination: format!(":local:{}", destination_dir.display()),
            interval_minutes: 60,
            enabled: true,
            last_run_at: None,
            next_run_at: None,
            status: "ready".into(),
            last_message: None,
            created_at: now_string(),
        };

        let result = perform_copy(&job);
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
            enabled: true,
            last_run_at: None,
            next_run_at: None,
            status: "ready".into(),
            last_message: None,
            created_at: now_string(),
        };

        let result = perform_copy(&job);
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
}
