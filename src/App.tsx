import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

type JobStatus = "ready" | "running" | "success" | "error" | "paused";
type BackupMode = "full" | "incremental" | "differential" | "mirror";
type CloudSetupStatus =
  | "ready"
  | "connecting"
  | "success"
  | "error"
  | "advanced";

interface SyncJob {
  id: number;
  name: string;
  source_paths: string[];
  destination: string;
  interval_minutes: number;
  backup_mode: BackupMode;
  last_full_at: string | null;
  enabled: boolean;
  last_run_at: string | null;
  next_run_at: string | null;
  status: JobStatus;
  last_message: string | null;
  progress_percent: number;
  progress_message: string | null;
  retention_count: number;
  created_at: string;
}

interface RunRecord {
  id: number;
  job_id: number;
  started_at: string;
  finished_at: string;
  status: string;
  message: string;
}

interface ErrorLog {
  id: number;
  job_id: number;
  job_name: string;
  started_at: string;
  finished_at: string;
  message: string;
}

interface VersionSnapshot {
  name: string;
  created_at: string;
}

interface VersionFile {
  path: string;
  size: number;
  modified_at: string | null;
}

interface RestoredVersion {
  path: string;
  message: string;
}

interface BackgroundServiceStatus {
  installed: boolean;
  enabled: boolean;
  active: boolean;
  message: string;
}

interface UpdateInfo {
  available: boolean;
  current_version: string;
  latest_version: string;
  title: string;
  notes: string;
  release_url: string;
  published_at: string | null;
  asset_name: string | null;
  download_url: string | null;
  download_size: number | null;
  package_type: "deb" | "appimage";
}

interface DownloadedUpdate {
  path: string;
  instructions: string;
}

type UpdaterStatus =
  | "idle"
  | "checking"
  | "current"
  | "available"
  | "downloading"
  | "ready"
  | "error";

type IntervalUnit = "minutes" | "hours" | "days";

interface JobDraft {
  name: string;
  source_paths: string[];
  remote: string;
  cloud_path: string | null;
  interval_minutes: number;
  backup_mode: BackupMode;
  retention_count: number;
}

interface CloudFolderEntry {
  name: string;
  path: string;
}

const initialDraft: JobDraft = {
  name: "",
  source_paths: [],
  remote: "",
  cloud_path: null,
  interval_minutes: 60,
  backup_mode: "incremental",
  retention_count: 5,
};

function formatTime(value: string | null): string {
  if (!value) return "Not yet";
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(value));
}

function formatInterval(minutes: number): string {
  if (minutes < 60) return `Every ${minutes} minutes`;
  if (minutes === 60) return "Every hour";
  if (minutes % 1440 === 0) {
    const days = minutes / 1440;
    return days === 1 ? "Every day" : `Every ${days} days`;
  }
  if (minutes % 60 === 0) return `Every ${minutes / 60} hours`;
  return `Every ${minutes} minutes`;
}

function intervalMultiplier(unit: IntervalUnit): number {
  if (unit === "hours") return 60;
  if (unit === "days") return 1440;
  return 1;
}

function intervalValue(minutes: number, unit: IntervalUnit): number {
  return Math.max(1, Math.round(minutes / intervalMultiplier(unit)));
}

function shortPath(path: string): string {
  if (path.length <= 46) return path;
  return `…${path.slice(-45)}`;
}

function sourceSummary(paths: string[]): string {
  if (paths.length === 0) return "No files or folders selected";
  if (paths.length === 1) return shortPath(paths[0]);
  return `${paths.length} files and folders`;
}

function backupModeLabel(mode: BackupMode): string {
  if (mode === "full") return "Full backup";
  if (mode === "differential") return "Differential backup";
  if (mode === "mirror") return "Mirroring";
  return "Incremental backup";
}

function formatFileSize(bytes: number | null): string {
  if (!bytes) return "";
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

export default function App() {
  const [jobs, setJobs] = useState<SyncJob[]>([]);
  const [remotes, setRemotes] = useState<string[]>([]);
  const [draft, setDraft] = useState<JobDraft>(initialDraft);
  const [showCreate, setShowCreate] = useState(false);
  const [editingJob, setEditingJob] = useState<SyncJob | null>(null);
  const [showCloudSetup, setShowCloudSetup] = useState(false);
  const [cloudSetupStatus, setCloudSetupStatus] =
    useState<CloudSetupStatus>("ready");
  const [cloudSetupMessage, setCloudSetupMessage] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [runningIds, setRunningIds] = useState<Set<number>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [selectedJob, setSelectedJob] = useState<SyncJob | null>(null);
  const [runs, setRuns] = useState<RunRecord[]>([]);
  const [versionJob, setVersionJob] = useState<SyncJob | null>(null);
  const [versionSnapshots, setVersionSnapshots] = useState<VersionSnapshot[]>(
    [],
  );
  const [selectedSnapshot, setSelectedSnapshot] =
    useState<VersionSnapshot | null>(null);
  const [versionFiles, setVersionFiles] = useState<VersionFile[]>([]);
  const [versionsLoading, setVersionsLoading] = useState(false);
  const [versionsError, setVersionsError] = useState<string | null>(null);
  const [restoringFile, setRestoringFile] = useState<string | null>(null);
  const [showErrorLog, setShowErrorLog] = useState(false);
  const [errorLogs, setErrorLogs] = useState<ErrorLog[]>([]);
  const [errorLogsLoading, setErrorLogsLoading] = useState(false);
  const [serviceStatus, setServiceStatus] = useState<BackgroundServiceStatus>({
    installed: false,
    enabled: false,
    active: false,
    message: "Checking the background service…",
  });
  const [showUpdater, setShowUpdater] = useState(false);
  const [updaterStatus, setUpdaterStatus] = useState<UpdaterStatus>("idle");
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [downloadedUpdate, setDownloadedUpdate] =
    useState<DownloadedUpdate | null>(null);
  const [updaterError, setUpdaterError] = useState<string | null>(null);
  const [showAdvancedSchedule, setShowAdvancedSchedule] = useState(false);
  const [mirrorAcknowledged, setMirrorAcknowledged] = useState(false);
  const [customInterval, setCustomInterval] = useState(1);
  const [customIntervalUnit, setCustomIntervalUnit] =
    useState<IntervalUnit>("hours");
  const [showFolderBrowser, setShowFolderBrowser] = useState(false);
  const [folderBrowserPath, setFolderBrowserPath] = useState("");
  const [cloudFolders, setCloudFolders] = useState<CloudFolderEntry[]>([]);
  const [folderLoading, setFolderLoading] = useState(false);
  const [folderError, setFolderError] = useState<string | null>(null);
  const [showNewFolder, setShowNewFolder] = useState(false);
  const [newFolderName, setNewFolderName] = useState("");
  const [creatingFolder, setCreatingFolder] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const [nextJobs, nextRemotes, nextErrorLogs, nextServiceStatus] =
        await Promise.all([
        invoke<SyncJob[]>("list_jobs"),
        invoke<string[]>("list_remotes"),
        invoke<ErrorLog[]>("list_error_logs"),
          invoke<BackgroundServiceStatus>("background_service_status"),
        ]);
      const safeJobs = nextJobs ?? [];
      const safeRemotes = nextRemotes ?? [];
      setJobs(safeJobs);
      setRemotes(safeRemotes);
      setErrorLogs(nextErrorLogs ?? []);
      if (nextServiceStatus) setServiceStatus(nextServiceStatus);
      setDraft((current) => ({
        ...current,
        remote: current.remote || safeRemotes[0] || "",
      }));
      setError(null);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setLoading(false);
    }
  }, []);

  const refreshJobs = useCallback(async () => {
    try {
      setJobs((await invoke<SyncJob[]>("list_jobs")) ?? []);
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), 15_000);
    return () => window.clearInterval(timer);
  }, [refresh]);

  useEffect(() => {
    const timer = window.setTimeout(() => void checkForUpdates(false), 5_000);
    return () => window.clearTimeout(timer);
  }, []);

  const activeJobs = useMemo(
    () => jobs.filter((job) => job.enabled).length,
    [jobs],
  );
  const hasRunningJob =
    runningIds.size > 0 || jobs.some((job) => job.status === "running");

  useEffect(() => {
    const timer = window.setInterval(
      () => void refreshJobs(),
      hasRunningJob ? 750 : 5_000,
    );
    return () => window.clearInterval(timer);
  }, [hasRunningJob, refreshJobs]);

  async function chooseSource(directory: boolean) {
    const selected = await open({ directory, multiple: true });
    if (!selected) return;
    const additions = Array.isArray(selected) ? selected : [selected];
    if (additions.length === 0) return;
    setDraft((current) => ({
      ...current,
      source_paths: Array.from(
        new Set([...current.source_paths, ...additions]),
      ),
      name:
        current.name ||
        (additions[0].split("/").filter(Boolean).pop() ?? "My backup"),
    }));
  }

  function removeSource(path: string) {
    setDraft((current) => ({
      ...current,
      source_paths: current.source_paths.filter((source) => source !== path),
    }));
  }

  function setAdvancedInterval(value: number, unit = customIntervalUnit) {
    const safeValue = Math.max(1, Math.floor(value || 1));
    setCustomInterval(safeValue);
    setDraft((current) => ({
      ...current,
      interval_minutes: safeValue * intervalMultiplier(unit),
    }));
  }

  function setAdvancedIntervalUnit(unit: IntervalUnit) {
    setCustomIntervalUnit(unit);
    setDraft((current) => ({
      ...current,
      interval_minutes: customInterval * intervalMultiplier(unit),
    }));
  }

  function toggleAdvancedSchedule() {
    if (!showAdvancedSchedule) {
      const unit: IntervalUnit =
        draft.interval_minutes % 1440 === 0
          ? "days"
          : draft.interval_minutes % 60 === 0
            ? "hours"
            : "minutes";
      setCustomIntervalUnit(unit);
      setCustomInterval(intervalValue(draft.interval_minutes, unit));
    }
    setShowAdvancedSchedule((current) => !current);
  }

  function setScheduleEditor(intervalMinutes: number) {
    const unit: IntervalUnit =
      intervalMinutes % 1440 === 0
        ? "days"
        : intervalMinutes % 60 === 0
          ? "hours"
          : "minutes";
    setCustomIntervalUnit(unit);
    setCustomInterval(intervalValue(intervalMinutes, unit));
    setShowAdvancedSchedule(
      ![15, 30, 60, 180, 360, 720, 1440].includes(intervalMinutes),
    );
  }

  function openNewJob() {
    setEditingJob(null);
    setDraft({ ...initialDraft, remote: remotes[0] || "" });
    setScheduleEditor(initialDraft.interval_minutes);
    setMirrorAcknowledged(false);
    setShowCreate(true);
  }

  function openEditJob(job: SyncJob) {
    const matchingRemote = remotes.find((remote) =>
      job.destination.startsWith(remote),
    );
    const colonIndex = job.destination.indexOf(":");
    const remote =
      matchingRemote ||
      (colonIndex >= 0 ? job.destination.slice(0, colonIndex + 1) : "");
    setEditingJob(job);
    setDraft({
      name: job.name,
      source_paths: [...job.source_paths],
      remote,
      cloud_path: job.destination.slice(remote.length),
      interval_minutes: job.interval_minutes,
      backup_mode: job.backup_mode,
      retention_count: job.retention_count,
    });
    setScheduleEditor(job.interval_minutes);
    setMirrorAcknowledged(job.backup_mode === "mirror");
    setSelectedJob(null);
    setShowCreate(true);
  }

  function closeJobForm() {
    setShowCreate(false);
    setEditingJob(null);
  }

  async function saveJob(event: React.FormEvent) {
    event.preventDefault();
    setSaving(true);
    setError(null);
    try {
      const cleanCloudPath = (draft.cloud_path ?? "").replace(/^\/+/, "");
      const input = {
        name: draft.name,
        source_paths: draft.source_paths,
        destination: `${draft.remote}${cleanCloudPath}`,
        interval_minutes: Number(draft.interval_minutes),
        backup_mode: draft.backup_mode,
        retention_count: Number(draft.retention_count),
      };
      await invoke<SyncJob>(editingJob ? "update_job" : "create_job", {
        ...(editingJob ? { jobId: editingJob.id } : {}),
        input,
      });
      const successMessage = editingJob
        ? `${draft.name.trim()} was updated.`
        : "Backup job created. It will run on its schedule.";
      setDraft({ ...initialDraft, remote: remotes[0] || "" });
      setShowAdvancedSchedule(false);
      setCustomInterval(1);
      setCustomIntervalUnit("hours");
      setMirrorAcknowledged(false);
      setShowCreate(false);
      setEditingJob(null);
      setNotice(successMessage);
      await refresh();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setSaving(false);
    }
  }

  async function loadCloudFolders(path: string) {
    if (!draft.remote) {
      setFolderError("Connect Google Drive first.");
      return;
    }
    setFolderLoading(true);
    setFolderError(null);
    try {
      const folders = await invoke<CloudFolderEntry[]>("list_cloud_folders", {
        remote: draft.remote,
        path,
      });
      setCloudFolders(folders);
      setFolderBrowserPath(path);
    } catch (reason) {
      setFolderError(String(reason));
      setCloudFolders([]);
    } finally {
      setFolderLoading(false);
    }
  }

  function openFolderBrowser() {
    const startingPath = draft.cloud_path ?? "";
    setShowFolderBrowser(true);
    setShowNewFolder(false);
    setNewFolderName("");
    void loadCloudFolders(startingPath);
  }

  function folderParent(path: string): string {
    const pieces = path.split("/").filter(Boolean);
    pieces.pop();
    return pieces.join("/");
  }

  async function createDriveFolder(event: React.FormEvent) {
    event.preventDefault();
    setCreatingFolder(true);
    setFolderError(null);
    try {
      const createdPath = await invoke<string>("create_cloud_folder", {
        remote: draft.remote,
        path: folderBrowserPath,
        name: newFolderName,
      });
      setNewFolderName("");
      setShowNewFolder(false);
      await loadCloudFolders(createdPath);
    } catch (reason) {
      setFolderError(String(reason));
    } finally {
      setCreatingFolder(false);
    }
  }

  async function runJob(job: SyncJob) {
    setRunningIds((current) => new Set(current).add(job.id));
    setError(null);
    setNotice(null);
    try {
      const result = await invoke<RunRecord>("run_job", { jobId: job.id });
      setNotice(
        result.status === "success"
          ? `${job.name} is safely backed up.`
          : `${job.name} finished with an error.`,
      );
      await refresh();
      if (selectedJob?.id === job.id) await showHistory(job);
    } catch (reason) {
      setError(String(reason));
      setNotice(null);
    } finally {
      setRunningIds((current) => {
        const next = new Set(current);
        next.delete(job.id);
        return next;
      });
    }
  }

  async function setEnabled(job: SyncJob, enabled: boolean) {
    try {
      await invoke<SyncJob>("set_job_enabled", { jobId: job.id, enabled });
      await refresh();
    } catch (reason) {
      setError(String(reason));
    }
  }

  async function removeJob(job: SyncJob) {
    if (!window.confirm(`Remove “${job.name}”? Cloud files will not be deleted.`)) {
      return;
    }
    try {
      await invoke("delete_job", { jobId: job.id });
      setSelectedJob(null);
      await refresh();
      setNotice(`${job.name} was removed. Its cloud files were left untouched.`);
    } catch (reason) {
      setError(String(reason));
    }
  }

  async function showHistory(job: SyncJob) {
    setSelectedJob(job);
    try {
      setRuns(await invoke<RunRecord[]>("job_history", { jobId: job.id }));
    } catch (reason) {
      setError(String(reason));
    }
  }

  async function loadVersionFiles(
    job: SyncJob,
    snapshot: VersionSnapshot,
  ) {
    setSelectedSnapshot(snapshot);
    setVersionsLoading(true);
    setVersionsError(null);
    try {
      setVersionFiles(
        (await invoke<VersionFile[]>("list_version_files", {
          jobId: job.id,
          snapshotName: snapshot.name,
        })) ?? [],
      );
    } catch (reason) {
      setVersionFiles([]);
      setVersionsError(String(reason));
    } finally {
      setVersionsLoading(false);
    }
  }

  async function openPreviousFiles(job: SyncJob) {
    setSelectedJob(null);
    setVersionJob(job);
    setVersionSnapshots([]);
    setSelectedSnapshot(null);
    setVersionFiles([]);
    setVersionsLoading(true);
    setVersionsError(null);
    try {
      const snapshots =
        (await invoke<VersionSnapshot[]>("list_version_snapshots", {
          jobId: job.id,
        })) ?? [];
      setVersionSnapshots(snapshots);
      if (snapshots.length > 0) {
        await loadVersionFiles(job, snapshots[0]);
      }
    } catch (reason) {
      setVersionsError(String(reason));
      setVersionsLoading(false);
    }
  }

  function closePreviousFiles() {
    setVersionJob(null);
    setVersionSnapshots([]);
    setSelectedSnapshot(null);
    setVersionFiles([]);
    setVersionsError(null);
    setRestoringFile(null);
  }

  async function restorePreviousFile(file: VersionFile) {
    if (!versionJob || !selectedSnapshot) return;
    const selected = await open({
      directory: true,
      multiple: false,
      title: "Choose where to put the restored file",
    });
    if (!selected) return;
    const destination = Array.isArray(selected) ? selected[0] : selected;
    if (!destination) return;
    setRestoringFile(file.path);
    setVersionsError(null);
    try {
      const restored = await invoke<RestoredVersion>("restore_version_file", {
        jobId: versionJob.id,
        snapshotName: selectedSnapshot.name,
        filePath: file.path,
        destinationDir: destination,
      });
      setNotice(`${restored.message} Saved to ${shortPath(restored.path)}`);
    } catch (reason) {
      setVersionsError(String(reason));
    } finally {
      setRestoringFile(null);
    }
  }

  async function openErrorLog() {
    setShowErrorLog(true);
    setErrorLogsLoading(true);
    try {
      setErrorLogs((await invoke<ErrorLog[]>("list_error_logs")) ?? []);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setErrorLogsLoading(false);
    }
  }

  async function copyErrorReport() {
    const report = errorLogs
      .map(
        (entry) =>
          `[${entry.finished_at}] ${entry.job_name}\n${entry.message}`,
      )
      .join("\n\n");
    try {
      await navigator.clipboard.writeText(report);
      setNotice("The error report was copied. You can paste it into a message.");
    } catch {
      setError("CloudFolder could not copy the error report.");
    }
  }

  async function repairBackgroundService() {
    try {
      const status = await invoke<BackgroundServiceStatus>(
        "repair_background_service",
      );
      setServiceStatus(status);
      setNotice("Background backups are now running automatically.");
    } catch (reason) {
      setError(String(reason));
    }
  }

  async function checkForUpdates(showResult = true) {
    if (showResult) setShowUpdater(true);
    setUpdaterStatus("checking");
    setUpdaterError(null);
    setDownloadedUpdate(null);
    try {
      const info = await invoke<UpdateInfo>("check_for_updates");
      if (!info) {
        if (showResult) {
          setUpdaterStatus("error");
          setUpdaterError("CloudFolder could not read the update information.");
        } else {
          setUpdaterStatus("idle");
        }
        return;
      }
      setUpdateInfo(info);
      setUpdaterStatus(info.available ? "available" : "current");
      if (info.available) setShowUpdater(true);
    } catch (reason) {
      if (showResult) {
        setUpdaterStatus("error");
        setUpdaterError(String(reason));
      } else {
        setUpdaterStatus("idle");
      }
    }
  }

  async function downloadAvailableUpdate() {
    if (!updateInfo?.download_url || !updateInfo.asset_name) return;
    setUpdaterStatus("downloading");
    setUpdaterError(null);
    try {
      const downloaded = await invoke<DownloadedUpdate>("download_update", {
        downloadUrl: updateInfo.download_url,
        assetName: updateInfo.asset_name,
      });
      setDownloadedUpdate(downloaded);
      setUpdaterStatus("ready");
    } catch (reason) {
      setUpdaterStatus("error");
      setUpdaterError(String(reason));
    }
  }

  async function openUpdateReleasePage() {
    if (!updateInfo) return;
    try {
      await invoke("open_release_page", {
        releaseUrl: updateInfo.release_url,
      });
    } catch (reason) {
      setUpdaterStatus("error");
      setUpdaterError(String(reason));
    }
  }

  function configureCloud() {
    const alreadyConnected = remotes.includes("CloudFolder:");
    setCloudSetupStatus(alreadyConnected ? "success" : "ready");
    setCloudSetupMessage(null);
    setShowCloudSetup(true);
  }

  async function connectGoogleDrive() {
    setCloudSetupStatus("connecting");
    setCloudSetupMessage(null);
    setError(null);
    try {
      await invoke<string>("connect_google_drive");
      await refresh();
      setCloudSetupStatus("success");
    } catch (reason) {
      setCloudSetupStatus("error");
      setCloudSetupMessage(String(reason));
    }
  }

  async function openAdvancedCloudSetup() {
    try {
      await invoke("open_rclone_config");
      setCloudSetupStatus("advanced");
      setCloudSetupMessage(null);
    } catch (reason) {
      setCloudSetupStatus("error");
      setCloudSetupMessage(String(reason));
    }
  }

  async function openSupportPage() {
    try {
      await invoke("open_support_page");
    } catch (reason) {
      setError(String(reason));
    }
  }

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-mark" aria-hidden="true">
            <span />
            <span />
          </div>
          <div>
            <strong>CloudFolder</strong>
            <small>Safe Ubuntu backup</small>
          </div>
        </div>

        <nav aria-label="Main navigation">
          <button className="nav-item active">
            <span>⌂</span> Backups
            <b>{jobs.length}</b>
          </button>
          <button className="nav-item" onClick={() => configureCloud()}>
            <span>☁</span> Google Drive
          </button>
          <button className="nav-item" onClick={() => void openErrorLog()}>
            <span>!</span> Error log
            {errorLogs.length > 0 && <b>{errorLogs.length}</b>}
          </button>
          <button
            className="nav-item"
            onClick={() => void checkForUpdates(true)}
          >
            <span>↓</span> Updates
            {updateInfo?.available && <b>New</b>}
          </button>
        </nav>

        <div className="safety-note">
          <span className="shield">✓</span>
          <div>
            <strong>Safe by default</strong>
            <p>Only Mirroring can delete cloud files, after a warning.</p>
          </div>
        </div>

        <button className="support-link" onClick={() => void openSupportPage()}>
          <span aria-hidden="true">♥</span>
          <div>
            <strong>Support CloudFolder</strong>
            <small>Visit Ryan’s Ko-fi page</small>
          </div>
          <b aria-hidden="true">↗</b>
        </button>

        <button className="quit-link" onClick={() => void invoke("quit_app")}>
          Close app — backups stay on
        </button>
      </aside>

      <main>
        <header className="topbar">
          <div>
            <p className="eyebrow">This computer</p>
            <h1>Your backups</h1>
          </div>
          <button className="primary" onClick={openNewJob}>
            <span>＋</span> New backup
          </button>
        </header>

        <section className="summary-grid" aria-label="Backup summary">
          <article>
            <span className="summary-icon green">↻</span>
            <div>
              <strong>{activeJobs}</strong>
              <small>Active schedules</small>
            </div>
          </article>
          <article>
            <span className="summary-icon blue">☁</span>
            <div>
              <strong>{remotes.length}</strong>
              <small>Cloud connections</small>
            </div>
          </article>
          <article>
            <span className="summary-icon amber">◷</span>
            <div>
              <strong>
                {jobs.some((job) => job.status === "error") ? "Needs attention" : "All clear"}
              </strong>
              <small>System status</small>
            </div>
          </article>
        </section>

        <section
          className={`background-service-card ${
            serviceStatus.active ? "active" : "inactive"
          }`}
          aria-label="Background backup service"
        >
          <span aria-hidden="true">{serviceStatus.active ? "✓" : "!"}</span>
          <div>
            <strong>
              {serviceStatus.active
                ? "Background backups are running"
                : "Background backups need attention"}
            </strong>
            <p>{serviceStatus.message}</p>
          </div>
          {!serviceStatus.active && (
            <button
              className="secondary"
              onClick={() => void repairBackgroundService()}
            >
              Turn on background backups
            </button>
          )}
        </section>

        {error && (
          <div className="banner error-banner">
            <span>!</span>
            <p>{error}</p>
            <button onClick={() => setError(null)}>Dismiss</button>
          </div>
        )}
        {notice && (
          <div className="banner notice-banner">
            <span>✓</span>
            <p>{notice}</p>
            <button onClick={() => setNotice(null)}>Dismiss</button>
          </div>
        )}

        {remotes.length === 0 && !loading && (
          <section className="connect-card">
            <div className="cloud-illustration">☁</div>
            <div>
              <p className="eyebrow">One-time setup</p>
              <h2>Connect Google Drive</h2>
              <p>
                Press one button, choose your Google account, and you are done.
                CloudFolder never sees your Google password.
              </p>
            </div>
            <button className="primary" onClick={() => configureCloud()}>
              Add Google Drive
            </button>
          </section>
        )}

        <section className="jobs-section">
          <div className="section-heading">
            <div>
              <h2>Backup jobs</h2>
              <p>Files are checked automatically, even while this window is closed.</p>
            </div>
            <button className="refresh" onClick={() => void refresh()}>
              ↻ Refresh
            </button>
          </div>

          {loading ? (
            <div className="empty-state">Loading your backups…</div>
          ) : jobs.length === 0 ? (
            <div className="empty-state">
              <span>＋</span>
              <h3>No backups yet</h3>
              <p>Choose a folder or file and protect it with an automatic schedule.</p>
              <button className="primary" onClick={openNewJob}>
                Create your first backup
              </button>
            </div>
          ) : (
            <div className="job-list">
              {jobs.map((job) => {
                const isRunning = runningIds.has(job.id) || job.status === "running";
                const progressPercent =
                  runningIds.has(job.id) && job.status !== "running"
                    ? 0
                    : Math.max(0, Math.min(100, job.progress_percent ?? 0));
                return (
                  <article className="job-card" key={job.id}>
                    <button
                      className="job-main"
                      onClick={() => void showHistory(job)}
                    >
                      <span
                        className={`status-dot ${
                          isRunning ? "running" : job.status
                        }`}
                      />
                      <div className="job-copy">
                        <div className="job-title-row">
                          <h3>{job.name}</h3>
                          <span
                            className={`status-pill ${
                              isRunning ? "running" : job.status
                            }`}
                          >
                            {isRunning ? "Running" : job.status}
                          </span>
                        </div>
                        <p title={job.source_paths.join("\n")}>
                          {sourceSummary(job.source_paths)}
                        </p>
                        <div className="job-meta">
                          <span>☁ {job.destination}</span>
                          <span>▣ {backupModeLabel(job.backup_mode)}</span>
                          <span>◷ {formatInterval(job.interval_minutes)}</span>
                          <span>
                            {job.last_run_at
                              ? `Last run ${formatTime(job.last_run_at)}`
                              : `Next run ${formatTime(job.next_run_at)}`}
                          </span>
                        </div>
                      </div>
                    </button>
                    <div className="job-actions">
                      <label className="switch" title="Enable scheduled backup">
                        <input
                          type="checkbox"
                          checked={job.enabled}
                          onChange={(event) =>
                            void setEnabled(job, event.target.checked)
                          }
                        />
                        <span />
                      </label>
                      {isRunning ? (
                        <div
                          className="backup-progress"
                          role="progressbar"
                          aria-label={`Backing up ${job.name}`}
                          aria-valuemin={0}
                          aria-valuemax={100}
                          aria-valuenow={progressPercent}
                        >
                          <div className="backup-progress-copy">
                            <small>
                              {job.progress_message ??
                                `Backing up ${job.name}`}
                            </small>
                            <strong>{progressPercent}%</strong>
                          </div>
                          <div className="backup-progress-track">
                            <span
                              style={{ width: `${progressPercent}%` }}
                            />
                          </div>
                        </div>
                      ) : (
                        <button
                          className="run-button"
                          onClick={() => void runJob(job)}
                        >
                          Back up now
                        </button>
                      )}
                    </div>
                  </article>
                );
              })}
            </div>
          )}
        </section>
      </main>

      {showCloudSetup && (
        <div
          className="modal-backdrop"
          onMouseDown={() => {
            if (cloudSetupStatus !== "connecting") setShowCloudSetup(false);
          }}
        >
          <section
            className="modal cloud-setup-modal"
            role="dialog"
            aria-modal="true"
            aria-labelledby="cloud-setup-title"
            onMouseDown={(event) => event.stopPropagation()}
          >
            {cloudSetupStatus !== "connecting" && (
              <button
                className="modal-close"
                aria-label="Close"
                onClick={() => setShowCloudSetup(false)}
              >
                ×
              </button>
            )}

            {cloudSetupStatus === "success" ? (
              <div className="cloud-success">
                <div className="big-success" aria-hidden="true">✓</div>
                <p className="eyebrow">All done</p>
                <h2 id="cloud-setup-title">Google Drive is ready!</h2>
                <p>
                  CloudFolder can now put your backup files safely in Google Drive.
                </p>
                <button
                  className="primary giant-button"
                  onClick={() => setShowCloudSetup(false)}
                >
                  Great, let’s make a backup
                </button>
              </div>
            ) : cloudSetupStatus === "connecting" ? (
              <div className="cloud-waiting">
                <div className="friendly-spinner" aria-hidden="true">
                  <span>☁</span>
                </div>
                <p className="eyebrow">Almost there</p>
                <h2 id="cloud-setup-title">Look at your web browser</h2>
                <p className="large-help">
                  Choose your Google account and press <strong>Allow</strong>.
                  Then come back here.
                </p>
                <div className="waiting-note">
                  <span className="tiny-spinner" />
                  This page will finish by itself.
                </div>
              </div>
            ) : cloudSetupStatus === "advanced" ? (
              <div className="cloud-waiting">
                <div className="cloud-setup-icon" aria-hidden="true">⌨</div>
                <p className="eyebrow">Helper window</p>
                <h2 id="cloud-setup-title">Follow the terminal helper</h2>
                <p className="large-help">
                  Keep choosing the suggested answer. When it finishes, close this
                  box and press Google Drive again.
                </p>
                <button
                  className="primary giant-button"
                  onClick={async () => {
                    const nextRemotes = await invoke<string[]>("list_remotes");
                    setRemotes(nextRemotes);
                    if (nextRemotes.length > 0) {
                      setCloudSetupStatus("success");
                    } else {
                      setCloudSetupStatus("ready");
                      setCloudSetupMessage(
                        "Google Drive is not connected yet. Try the easy setup above.",
                      );
                    }
                  }}
                >
                  Check again
                </button>
              </div>
            ) : (
              <>
                <div className="cloud-setup-heading">
                  <div className="cloud-setup-icon" aria-hidden="true">☁</div>
                  <div>
                    <p className="eyebrow">Easy setup</p>
                    <h2 id="cloud-setup-title">Add Google Drive</h2>
                    <p>There are only three little steps.</p>
                  </div>
                </div>

                <ol className="kid-steps">
                  <li>
                    <span>1</span>
                    <div>
                      <strong>Press the big green button</strong>
                      <small>Your web browser will open.</small>
                    </div>
                  </li>
                  <li>
                    <span>2</span>
                    <div>
                      <strong>Choose your Google account</strong>
                      <small>Google may ask you to press Allow.</small>
                    </div>
                  </li>
                  <li>
                    <span>3</span>
                    <div>
                      <strong>Come back to CloudFolder</strong>
                      <small>We will tell you when everything is ready.</small>
                    </div>
                  </li>
                </ol>

                {cloudSetupStatus === "error" && (
                  <div className="simple-error">
                    <strong>That did not work yet.</strong>
                    <p>{cloudSetupMessage}</p>
                  </div>
                )}

                <button
                  className="primary giant-button"
                  onClick={() => void connectGoogleDrive()}
                >
                  {cloudSetupStatus === "error"
                    ? "Try connecting again"
                    : "Connect my Google Drive"}
                </button>
                <div className="privacy-promise">
                  <span>✓</span>
                  <p>
                    <strong>Your password stays private.</strong>
                    Google lets CloudFolder browse folders and upload backups.
                    CloudFolder never deletes Drive files.
                  </p>
                </div>
                <button
                  className="advanced-link"
                  onClick={() => void openAdvancedCloudSetup()}
                >
                  Advanced setup for grown-ups
                </button>
              </>
            )}
          </section>
        </div>
      )}

      {showCreate && (
        <div className="modal-backdrop" onMouseDown={closeJobForm}>
          <section
            className="modal"
            role="dialog"
            aria-modal="true"
            aria-labelledby="create-title"
            onMouseDown={(event) => event.stopPropagation()}
          >
            <button className="modal-close" onClick={closeJobForm}>
              ×
            </button>
            <p className="eyebrow">
              {editingJob ? "Edit scheduled backup" : "New scheduled backup"}
            </p>
            <h2 id="create-title">
              {editingJob ? "Update this backup" : "Protect something important"}
            </h2>
            <p className="modal-intro">
              {editingJob
                ? "Change what is backed up, where it goes, or how often it runs."
                : "Choose a safe backup type below. CloudFolder warns before any option can delete cloud files."}
            </p>

            <form onSubmit={saveJob}>
              <label>
                Backup name
                <input
                  value={draft.name}
                  required
                  placeholder="Work documents"
                  onChange={(event) =>
                    setDraft({ ...draft, name: event.target.value })
                  }
                />
              </label>

              <fieldset>
                <legend>What should be backed up?</legend>
                <div className="source-picker multi-source-picker">
                  <div>
                    <strong>{sourceSummary(draft.source_paths)}</strong>
                    <small>Add as many as you need, up to 50</small>
                  </div>
                  <button type="button" onClick={() => void chooseSource(false)}>
                    ＋ Add files
                  </button>
                  <button type="button" onClick={() => void chooseSource(true)}>
                    ＋ Add folders
                  </button>
                </div>
                {draft.source_paths.length > 0 && (
                  <div className="selected-source-list">
                    {draft.source_paths.map((path) => (
                      <div key={path}>
                        <span aria-hidden="true">▰</span>
                        <strong title={path}>{shortPath(path)}</strong>
                        <button
                          type="button"
                          aria-label={`Remove ${path}`}
                          onClick={() => removeSource(path)}
                        >
                          ×
                        </button>
                      </div>
                    ))}
                  </div>
                )}
                {draft.source_paths.length > 1 && (
                  <p className="multi-source-note">
                    Each item gets its own named folder in Google Drive, so files
                    cannot overwrite each other.
                  </p>
                )}
              </fieldset>

              <div className="form-row">
                <label>
                  Google account
                  <select
                    value={draft.remote}
                    required
                    onChange={(event) =>
                      setDraft({
                        ...draft,
                        remote: event.target.value,
                        cloud_path: null,
                      })
                    }
                  >
                    <option value="" disabled>
                      Choose an account
                    </option>
                    {draft.remote && !remotes.includes(draft.remote) && (
                      <option value={draft.remote}>
                        {draft.remote.replace(/:$/, "")}
                      </option>
                    )}
                    {remotes.map((remote) => (
                      <option value={remote} key={remote}>
                        {remote.replace(/:$/, "")}
                      </option>
                    ))}
                  </select>
                </label>
                <div className="field-group">
                  <span className="field-label">Google Drive folder</span>
                  <button
                    type="button"
                    className="cloud-folder-field"
                    disabled={!draft.remote}
                    onClick={openFolderBrowser}
                  >
                    <span className="folder-glyph" aria-hidden="true">▰</span>
                    <span>
                      <strong>
                        {draft.cloud_path === null
                          ? "Choose a folder"
                          : draft.cloud_path || "My Drive"}
                      </strong>
                      <small>Click to browse Google Drive</small>
                    </span>
                    <b aria-hidden="true">›</b>
                  </button>
                </div>
              </div>

              {remotes.length === 0 && (
                <button
                  type="button"
                  className="inline-setup"
                  onClick={() => configureCloud()}
                >
                  Add Google Drive first
                </button>
              )}

              <fieldset className="backup-mode-fieldset">
                <legend>Backup type</legend>
                <div className="backup-mode-grid">
                  {([
                    [
                      "full",
                      "Full backup",
                      "Save a new dated copy of everything each time.",
                    ],
                    [
                      "incremental",
                      "Incremental backup",
                      "Safely upload only new and changed files.",
                    ],
                    [
                      "differential",
                      "Differential backup",
                      "Keep a full baseline, then dated changes since it.",
                    ],
                    [
                      "mirror",
                      "Mirroring",
                      "Make a cloud folder exactly match a local folder.",
                    ],
                  ] as const).map(([mode, title, description]) => (
                    <label
                      className={`backup-mode-card ${
                        draft.backup_mode === mode ? "selected" : ""
                      } ${mode === "mirror" ? "destructive" : ""}`}
                      key={mode}
                    >
                      <input
                        type="radio"
                        name="backup-mode"
                        value={mode}
                        checked={draft.backup_mode === mode}
                        onChange={() => {
                          setDraft({ ...draft, backup_mode: mode });
                          if (mode !== "mirror") setMirrorAcknowledged(false);
                        }}
                      />
                      <span aria-hidden="true">
                        {mode === "full"
                          ? "▣"
                          : mode === "incremental"
                            ? "＋"
                            : mode === "differential"
                              ? "◫"
                              : "⇄"}
                      </span>
                      <strong>{title}</strong>
                      <small>{description}</small>
                    </label>
                  ))}
                </div>
                {draft.backup_mode === "mirror" && (
                  <label className="mirror-warning">
                    <input
                      type="checkbox"
                      checked={mirrorAcknowledged}
                      onChange={(event) =>
                        setMirrorAcknowledged(event.target.checked)
                      }
                    />
                    <span>
                      <strong>Mirroring can delete cloud files.</strong>
                      I understand that cloud files missing from this computer
                      will be removed from the selected destination. Mirroring
                      works with folders, not individual files.
                    </span>
                  </label>
                )}
              </fieldset>

              {draft.backup_mode === "incremental" ||
              draft.backup_mode === "mirror" ? (
                <fieldset className="retention-fieldset">
                  <legend>Previous files</legend>
                  <label className="retention-toggle">
                    <input
                      type="checkbox"
                      checked={draft.retention_count > 0}
                      onChange={(event) =>
                        setDraft({
                          ...draft,
                          retention_count: event.target.checked ? 5 : 0,
                        })
                      }
                    />
                    <span>
                      <strong>Keep a safety copy when a file changes</strong>
                      <small>
                        Changed or deleted cloud files can be restored later.
                      </small>
                    </span>
                  </label>
                  {draft.retention_count > 0 && (
                    <div className="retention-count">
                      <label>
                        Keep the last
                        <input
                          aria-label="Number of previous backups to keep"
                          type="number"
                          min={1}
                          max={50}
                          value={draft.retention_count}
                          onChange={(event) =>
                            setDraft({
                              ...draft,
                              retention_count: Math.max(
                                1,
                                Math.min(50, Number(event.target.value) || 1),
                              ),
                            })
                          }
                        />
                        backup runs
                      </label>
                      <p>
                        Older safety copies are removed automatically. The live
                        backup is never counted.
                      </p>
                    </div>
                  )}
                </fieldset>
              ) : (
                <div className="dated-copy-note">
                  <span aria-hidden="true">◷</span>
                  <p>
                    <strong>This backup type already keeps dated copies.</strong>
                    Full and differential backups place older files in dated
                    cloud folders.
                  </p>
                </div>
              )}

              <fieldset className="schedule-fieldset">
                <legend>Run automatically</legend>
                {!showAdvancedSchedule ? (
                  <select
                    aria-label="Backup schedule"
                    value={draft.interval_minutes}
                    onChange={(event) =>
                      setDraft({
                        ...draft,
                        interval_minutes: Number(event.target.value),
                      })
                    }
                  >
                    <option value={15}>Every 15 minutes</option>
                    <option value={30}>Every 30 minutes</option>
                    <option value={60}>Every hour</option>
                    <option value={180}>Every 3 hours</option>
                    <option value={360}>Every 6 hours</option>
                    <option value={720}>Every 12 hours</option>
                    <option value={1440}>Every day</option>
                  </select>
                ) : (
                  <div className="advanced-schedule">
                    <div className="advanced-schedule-row">
                      <span>Run every</span>
                      <input
                        aria-label="Custom schedule amount"
                        type="number"
                        min={1}
                        max={
                          customIntervalUnit === "days"
                            ? 30
                            : customIntervalUnit === "hours"
                              ? 168
                              : 10080
                        }
                        value={customInterval}
                        onChange={(event) =>
                          setAdvancedInterval(Number(event.target.value))
                        }
                      />
                      <select
                        aria-label="Custom schedule unit"
                        value={customIntervalUnit}
                        onChange={(event) =>
                          setAdvancedIntervalUnit(
                            event.target.value as IntervalUnit,
                          )
                        }
                      >
                        <option value="minutes">minutes</option>
                        <option value="hours">hours</option>
                        <option value="days">days</option>
                      </select>
                    </div>
                    <div className="schedule-summary">
                      <span aria-hidden="true">◷</span>
                      <p>
                        <strong>{formatInterval(draft.interval_minutes)}</strong>
                        <small>
                          The next backup is scheduled after each run finishes.
                        </small>
                      </p>
                    </div>
                  </div>
                )}
                <button
                  type="button"
                  className="advanced-schedule-toggle"
                  onClick={toggleAdvancedSchedule}
                >
                  {showAdvancedSchedule
                    ? "Use simple schedule choices"
                    : "More schedule options"}
                </button>
              </fieldset>

              <div className="modal-actions">
                <button
                  type="button"
                  className="secondary"
                  onClick={closeJobForm}
                >
                  Cancel
                </button>
                <button
                  className="primary"
                  disabled={
                    saving ||
                    draft.source_paths.length === 0 ||
                    !draft.remote ||
                    draft.cloud_path === null ||
                    !draft.name.trim() ||
                    (draft.backup_mode === "mirror" && !mirrorAcknowledged)
                  }
                >
                  {saving
                    ? editingJob
                      ? "Saving…"
                      : "Creating…"
                    : editingJob
                      ? "Save changes"
                      : "Create backup"}
                </button>
              </div>
            </form>
          </section>
        </div>
      )}

      {showFolderBrowser && (
        <div
          className="modal-backdrop folder-browser-backdrop"
          onMouseDown={() => setShowFolderBrowser(false)}
        >
          <section
            className="modal folder-browser-modal"
            role="dialog"
            aria-modal="true"
            aria-labelledby="folder-browser-title"
            onMouseDown={(event) => event.stopPropagation()}
          >
            <button
              className="modal-close"
              aria-label="Close folder browser"
              onClick={() => setShowFolderBrowser(false)}
            >
              ×
            </button>

            <div className="folder-browser-heading">
              <div className="drive-folder-icon" aria-hidden="true">▰</div>
              <div>
                <p className="eyebrow">Google Drive</p>
                <h2 id="folder-browser-title">Choose a folder</h2>
                <p>Your backup will be placed inside this folder.</p>
              </div>
            </div>

            <nav className="folder-breadcrumbs" aria-label="Current Drive folder">
              <button
                className={folderBrowserPath ? "" : "current"}
                onClick={() => void loadCloudFolders("")}
              >
                My Drive
              </button>
              {folderBrowserPath
                .split("/")
                .filter(Boolean)
                .map((piece, index, pieces) => {
                  const crumbPath = pieces.slice(0, index + 1).join("/");
                  return (
                    <span key={crumbPath}>
                      <b>›</b>
                      <button
                        className={
                          index === pieces.length - 1 ? "current" : ""
                        }
                        onClick={() => void loadCloudFolders(crumbPath)}
                      >
                        {piece}
                      </button>
                    </span>
                  );
                })}
            </nav>

            <div className="folder-browser-tools">
              <button
                className="secondary"
                disabled={!folderBrowserPath || folderLoading}
                onClick={() =>
                  void loadCloudFolders(folderParent(folderBrowserPath))
                }
              >
                ← Back
              </button>
              <button
                className="secondary"
                disabled={folderLoading}
                onClick={() => setShowNewFolder((current) => !current)}
              >
                ＋ New folder
              </button>
            </div>

            {showNewFolder && (
              <form className="new-folder-form" onSubmit={createDriveFolder}>
                <label>
                  New folder name
                  <div>
                    <input
                      autoFocus
                      value={newFolderName}
                      placeholder="My backups"
                      onChange={(event) => setNewFolderName(event.target.value)}
                    />
                    <button
                      className="primary"
                      disabled={creatingFolder || !newFolderName.trim()}
                    >
                      {creatingFolder ? "Making…" : "Make folder"}
                    </button>
                  </div>
                </label>
              </form>
            )}

            {folderError && (
              <div className="simple-error folder-error">
                <strong>We could not open that folder.</strong>
                <p>{folderError}</p>
                <button onClick={() => void loadCloudFolders(folderBrowserPath)}>
                  Try again
                </button>
              </div>
            )}

            <div className="cloud-folder-list" aria-live="polite">
              {folderLoading ? (
                <div className="folder-loading">
                  <span className="tiny-spinner" />
                  Opening folder…
                </div>
              ) : cloudFolders.length === 0 && !folderError ? (
                <div className="no-cloud-folders">
                  <span aria-hidden="true">▰</span>
                  <strong>This folder is empty</strong>
                  <small>You can use it, or make a new folder.</small>
                </div>
              ) : (
                cloudFolders.map((folder) => (
                  <button
                    className="cloud-folder-row"
                    key={folder.path}
                    onClick={() => void loadCloudFolders(folder.path)}
                  >
                    <span aria-hidden="true">▰</span>
                    <strong>{folder.name}</strong>
                    <b aria-hidden="true">›</b>
                  </button>
                ))
              )}
            </div>

            <div className="folder-browser-footer">
              <div>
                <small>Selected folder</small>
                <strong>{folderBrowserPath || "My Drive"}</strong>
              </div>
              <button
                className="primary"
                disabled={folderLoading || Boolean(folderError)}
                onClick={() => {
                  setDraft({ ...draft, cloud_path: folderBrowserPath });
                  setShowFolderBrowser(false);
                }}
              >
                Use this folder
              </button>
            </div>
          </section>
        </div>
      )}

      {showUpdater && (
        <div
          className="modal-backdrop"
          onMouseDown={() => {
            if (updaterStatus !== "downloading") setShowUpdater(false);
          }}
        >
          <section
            className="modal updater-modal"
            role="dialog"
            aria-modal="true"
            aria-labelledby="updater-title"
            onMouseDown={(event) => event.stopPropagation()}
          >
            {updaterStatus !== "downloading" && (
              <button
                className="modal-close"
                aria-label="Close updater"
                onClick={() => setShowUpdater(false)}
              >
                ×
              </button>
            )}
            <div className="updater-heading">
              <div className="updater-icon" aria-hidden="true">↓</div>
              <div>
                <p className="eyebrow">CloudFolder updater</p>
                <h2 id="updater-title">
                  {updaterStatus === "available"
                    ? "A new version is ready"
                    : updaterStatus === "current"
                      ? "CloudFolder is up to date"
                      : updaterStatus === "ready"
                        ? "The update is downloaded"
                        : updaterStatus === "error"
                          ? "Update check needs help"
                          : "Checking for updates"}
                </h2>
              </div>
            </div>

            {updaterStatus === "checking" ||
            updaterStatus === "downloading" ? (
              <div className="updater-progress">
                <span className="tiny-spinner" />
                <strong>
                  {updaterStatus === "downloading"
                    ? "Downloading the update…"
                    : "Asking GitHub for the latest release…"}
                </strong>
                <small>
                  {updaterStatus === "downloading"
                    ? "Keep CloudFolder open until the download finishes."
                    : "This usually takes only a moment."}
                </small>
              </div>
            ) : updaterStatus === "available" && updateInfo ? (
              <>
                <div className="version-change">
                  <span>Installed: v{updateInfo.current_version}</span>
                  <b aria-hidden="true">→</b>
                  <strong>New: v{updateInfo.latest_version}</strong>
                </div>
                <div className="release-notes">
                  <strong>{updateInfo.title}</strong>
                  <pre>{updateInfo.notes}</pre>
                </div>
                <div className="update-package-note">
                  <span aria-hidden="true">⬡</span>
                  <p>
                    <strong>
                      {updateInfo.package_type === "deb"
                        ? "Ubuntu installer"
                        : "Portable AppImage"}
                    </strong>
                    <small>
                      {updateInfo.asset_name || "Release download"}{" "}
                      {formatFileSize(updateInfo.download_size)}
                    </small>
                  </p>
                </div>
                <div className="updater-actions">
                  <button
                    className="secondary"
                    onClick={() => void openUpdateReleasePage()}
                  >
                    View on GitHub
                  </button>
                  {updateInfo.download_url && updateInfo.asset_name ? (
                    <button
                      className="primary"
                      onClick={() => void downloadAvailableUpdate()}
                    >
                      Download update
                    </button>
                  ) : (
                    <button
                      className="primary"
                      onClick={() => void openUpdateReleasePage()}
                    >
                      Open release
                    </button>
                  )}
                </div>
              </>
            ) : updaterStatus === "ready" && downloadedUpdate ? (
              <div className="update-ready">
                <span aria-hidden="true">✓</span>
                <strong>Ready for the last step</strong>
                <p>{downloadedUpdate.instructions}</p>
                <code>{downloadedUpdate.path}</code>
                <button
                  className="primary"
                  onClick={() => setShowUpdater(false)}
                >
                  Got it
                </button>
              </div>
            ) : updaterStatus === "current" && updateInfo ? (
              <div className="update-current">
                <span aria-hidden="true">✓</span>
                <strong>You have version {updateInfo.current_version}</strong>
                <p>No newer published GitHub release was found.</p>
                <button
                  className="secondary"
                  onClick={() => void checkForUpdates(true)}
                >
                  Check again
                </button>
              </div>
            ) : (
              <div className="update-error">
                <span aria-hidden="true">!</span>
                <strong>CloudFolder could not check for an update.</strong>
                <p>{updaterError}</p>
                <button
                  className="primary"
                  onClick={() => void checkForUpdates(true)}
                >
                  Try again
                </button>
              </div>
            )}
          </section>
        </div>
      )}

      {showErrorLog && (
        <div className="modal-backdrop" onMouseDown={() => setShowErrorLog(false)}>
          <section
            className="modal error-log-modal"
            role="dialog"
            aria-modal="true"
            aria-labelledby="error-log-title"
            onMouseDown={(event) => event.stopPropagation()}
          >
            <button
              className="modal-close"
              aria-label="Close error log"
              onClick={() => setShowErrorLog(false)}
            >
              ×
            </button>
            <div className="error-log-heading">
              <div className="error-log-icon" aria-hidden="true">!</div>
              <div>
                <p className="eyebrow">Troubleshooting</p>
                <h2 id="error-log-title">Error log</h2>
                <p>
                  Backup failures stay here with their time and full error message.
                </p>
              </div>
            </div>

            <div className="error-log-toolbar">
              <span>
                {errorLogs.length === 0
                  ? "No errors saved"
                  : `${errorLogs.length} saved ${
                      errorLogs.length === 1 ? "error" : "errors"
                    }`}
              </span>
              <div>
                <button
                  className="secondary"
                  onClick={() => void openErrorLog()}
                  disabled={errorLogsLoading}
                >
                  {errorLogsLoading ? "Checking…" : "Refresh"}
                </button>
                <button
                  className="primary"
                  onClick={() => void copyErrorReport()}
                  disabled={errorLogs.length === 0}
                >
                  Copy error report
                </button>
              </div>
            </div>

            <div className="error-log-list">
              {errorLogsLoading && errorLogs.length === 0 ? (
                <div className="error-log-empty">Looking for errors…</div>
              ) : errorLogs.length === 0 ? (
                <div className="error-log-empty">
                  <span aria-hidden="true">✓</span>
                  <strong>Everything looks good</strong>
                  <p>Failed backup runs will appear here automatically.</p>
                </div>
              ) : (
                errorLogs.map((entry) => (
                  <article key={entry.id}>
                    <header>
                      <div>
                        <span className="status-dot error" />
                        <strong>{entry.job_name}</strong>
                      </div>
                      <time dateTime={entry.finished_at}>
                        {formatTime(entry.finished_at)}
                      </time>
                    </header>
                    <pre>{entry.message}</pre>
                  </article>
                ))
              )}
            </div>
            <p className="error-log-footnote">
              CloudFolder keeps the latest 100 backup errors on this computer.
            </p>
          </section>
        </div>
      )}

      {versionJob && (
        <div className="modal-backdrop" onMouseDown={closePreviousFiles}>
          <section
            className="modal versions-modal"
            role="dialog"
            aria-modal="true"
            aria-labelledby="versions-title"
            onMouseDown={(event) => event.stopPropagation()}
          >
            <button
              className="modal-close"
              aria-label="Close previous files"
              onClick={closePreviousFiles}
            >
              ×
            </button>
            <div className="versions-heading">
              <div className="versions-icon" aria-hidden="true">↶</div>
              <div>
                <p className="eyebrow">Safety copies</p>
                <h2 id="versions-title">Previous files for {versionJob.name}</h2>
                <p>
                  Choose an older file and restore it to a folder on this
                  computer.
                </p>
              </div>
            </div>

            <div className="restore-safety-note">
              <span aria-hidden="true">✓</span>
              <p>
                <strong>Your current files stay untouched.</strong>
                If the same filename already exists, CloudFolder gives the
                restored file a new name.
              </p>
            </div>

            {versionsError && (
              <div className="version-error" role="alert">
                <span>!</span>
                <p>{versionsError}</p>
              </div>
            )}

            {versionsLoading && versionSnapshots.length === 0 ? (
              <div className="versions-empty">
                <span className="tiny-spinner" />
                <strong>Looking for previous files…</strong>
              </div>
            ) : versionSnapshots.length === 0 ? (
              <div className="versions-empty">
                <span aria-hidden="true">◷</span>
                <strong>No previous files yet</strong>
                <p>
                  A safety copy appears here after a cloud file is changed or
                  deleted by a backup.
                </p>
              </div>
            ) : (
              <>
                <div className="snapshot-picker">
                  <strong>Backup run</strong>
                  <div>
                    {versionSnapshots.map((snapshot) => (
                      <button
                        className={
                          selectedSnapshot?.name === snapshot.name
                            ? "selected"
                            : ""
                        }
                        key={snapshot.name}
                        onClick={() =>
                          void loadVersionFiles(versionJob, snapshot)
                        }
                      >
                        <span>◷</span>
                        {formatTime(snapshot.created_at)}
                      </button>
                    ))}
                  </div>
                </div>

                <div className="version-file-list">
                  {versionsLoading ? (
                    <div className="versions-empty compact">
                      <span className="tiny-spinner" />
                      <strong>Opening this backup…</strong>
                    </div>
                  ) : versionFiles.length === 0 ? (
                    <div className="versions-empty compact">
                      <span aria-hidden="true">✓</span>
                      <strong>No files were replaced in this run</strong>
                    </div>
                  ) : (
                    versionFiles.map((file) => (
                      <article key={file.path}>
                        <span className="version-file-icon" aria-hidden="true">
                          ▤
                        </span>
                        <div>
                          <strong title={file.path}>{file.path}</strong>
                          <small>
                            {formatFileSize(file.size) || "Small file"}
                            {file.modified_at
                              ? ` · Changed ${formatTime(file.modified_at)}`
                              : ""}
                          </small>
                        </div>
                        <button
                          className="secondary"
                          disabled={restoringFile !== null}
                          onClick={() => void restorePreviousFile(file)}
                        >
                          {restoringFile === file.path
                            ? "Restoring…"
                            : "Restore"}
                        </button>
                      </article>
                    ))
                  )}
                </div>
              </>
            )}
          </section>
        </div>
      )}

      {selectedJob && (
        <div className="modal-backdrop" onMouseDown={() => setSelectedJob(null)}>
          <section
            className="modal history-modal"
            role="dialog"
            aria-modal="true"
            onMouseDown={(event) => event.stopPropagation()}
          >
            <button className="modal-close" onClick={() => setSelectedJob(null)}>
              ×
            </button>
            <p className="eyebrow">Backup details</p>
            <h2>{selectedJob.name}</h2>
            <dl className="job-details">
              <div>
                <dt>Sources</dt>
                <dd className="detail-source-list">
                  {selectedJob.source_paths.map((path) => (
                    <span key={path}>{path}</span>
                  ))}
                </dd>
              </div>
              <div>
                <dt>Destination</dt>
                <dd>{selectedJob.destination}</dd>
              </div>
              <div>
                <dt>Schedule</dt>
                <dd>{formatInterval(selectedJob.interval_minutes)}</dd>
              </div>
              <div>
                <dt>Backup type</dt>
                <dd>{backupModeLabel(selectedJob.backup_mode)}</dd>
              </div>
              <div>
                <dt>Previous files</dt>
                <dd>
                  {selectedJob.backup_mode === "incremental" ||
                  selectedJob.backup_mode === "mirror"
                    ? selectedJob.retention_count > 0
                      ? `Keep ${selectedJob.retention_count} backup runs`
                      : "Turned off"
                    : "Stored in dated backup folders"}
                </dd>
              </div>
              <div>
                <dt>Next run</dt>
                <dd>{formatTime(selectedJob.next_run_at)}</dd>
              </div>
            </dl>
            <h3>Recent activity</h3>
            <div className="run-list">
              {runs.length === 0 ? (
                <p>No runs yet.</p>
              ) : (
                runs.map((run) => (
                  <article key={run.id}>
                    <span className={`status-dot ${run.status}`} />
                    <div>
                      <strong>{run.status}</strong>
                      <small>{formatTime(run.finished_at)}</small>
                      <p>{run.message}</p>
                    </div>
                  </article>
                ))
              )}
            </div>
            <div className="job-detail-actions">
              <div className="job-detail-safe-actions">
                <button
                  className="secondary"
                  onClick={() => openEditJob(selectedJob)}
                >
                  Edit backup
                </button>
                {(selectedJob.backup_mode === "incremental" ||
                  selectedJob.backup_mode === "mirror") && (
                  <button
                    className="secondary previous-files-button"
                    disabled={selectedJob.retention_count === 0}
                    onClick={() => void openPreviousFiles(selectedJob)}
                  >
                    Previous files
                  </button>
                )}
              </div>
              <div className="danger-row">
                <button
                  className="danger"
                  onClick={() => void removeJob(selectedJob)}
                >
                  Remove backup job
                </button>
                <small>Cloud files will remain there.</small>
              </div>
            </div>
          </section>
        </div>
      )}
    </div>
  );
}
