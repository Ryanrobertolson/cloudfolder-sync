import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

type JobStatus = "ready" | "running" | "success" | "error" | "paused";
type CloudSetupStatus =
  | "ready"
  | "connecting"
  | "success"
  | "error"
  | "advanced";

interface SyncJob {
  id: number;
  name: string;
  source_path: string;
  destination: string;
  interval_minutes: number;
  enabled: boolean;
  last_run_at: string | null;
  next_run_at: string | null;
  status: JobStatus;
  last_message: string | null;
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

interface JobDraft {
  name: string;
  source_path: string;
  remote: string;
  cloud_path: string | null;
  interval_minutes: number;
}

interface CloudFolderEntry {
  name: string;
  path: string;
}

const initialDraft: JobDraft = {
  name: "",
  source_path: "",
  remote: "",
  cloud_path: null,
  interval_minutes: 60,
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

function shortPath(path: string): string {
  if (path.length <= 46) return path;
  return `…${path.slice(-45)}`;
}

export default function App() {
  const [jobs, setJobs] = useState<SyncJob[]>([]);
  const [remotes, setRemotes] = useState<string[]>([]);
  const [draft, setDraft] = useState<JobDraft>(initialDraft);
  const [showCreate, setShowCreate] = useState(false);
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
      const [nextJobs, nextRemotes] = await Promise.all([
        invoke<SyncJob[]>("list_jobs"),
        invoke<string[]>("list_remotes"),
      ]);
      setJobs(nextJobs);
      setRemotes(nextRemotes);
      setDraft((current) => ({
        ...current,
        remote: current.remote || nextRemotes[0] || "",
      }));
      setError(null);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), 15_000);
    return () => window.clearInterval(timer);
  }, [refresh]);

  const activeJobs = useMemo(
    () => jobs.filter((job) => job.enabled).length,
    [jobs],
  );

  async function chooseSource(directory: boolean) {
    const selected = await open({ directory, multiple: false });
    if (typeof selected !== "string") return;
    const fallbackName = selected.split("/").filter(Boolean).pop() ?? "My backup";
    setDraft((current) => ({
      ...current,
      source_path: selected,
      name: current.name || fallbackName,
    }));
  }

  async function createJob(event: React.FormEvent) {
    event.preventDefault();
    setSaving(true);
    setError(null);
    try {
      const cleanCloudPath = (draft.cloud_path ?? "").replace(/^\/+/, "");
      await invoke<SyncJob>("create_job", {
        input: {
          name: draft.name,
          source_path: draft.source_path,
          destination: `${draft.remote}${cleanCloudPath}`,
          interval_minutes: Number(draft.interval_minutes),
        },
      });
      setDraft({ ...initialDraft, remote: remotes[0] || "" });
      setShowCreate(false);
      setNotice("Backup job created. It will run on its schedule.");
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
    setNotice(`Backing up ${job.name}…`);
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
        </nav>

        <div className="safety-note">
          <span className="shield">✓</span>
          <div>
            <strong>Safe backup mode</strong>
            <p>Cloud files are never automatically deleted.</p>
          </div>
        </div>

        <button className="quit-link" onClick={() => void invoke("quit_app")}>
          Quit CloudFolder
        </button>
      </aside>

      <main>
        <header className="topbar">
          <div>
            <p className="eyebrow">This computer</p>
            <h1>Your backups</h1>
          </div>
          <button className="primary" onClick={() => setShowCreate(true)}>
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
              <p>Files are checked automatically while CloudFolder is running.</p>
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
              <button className="primary" onClick={() => setShowCreate(true)}>
                Create your first backup
              </button>
            </div>
          ) : (
            <div className="job-list">
              {jobs.map((job) => {
                const isRunning = runningIds.has(job.id) || job.status === "running";
                return (
                  <article className="job-card" key={job.id}>
                    <button
                      className="job-main"
                      onClick={() => void showHistory(job)}
                    >
                      <span className={`status-dot ${job.status}`} />
                      <div className="job-copy">
                        <div className="job-title-row">
                          <h3>{job.name}</h3>
                          <span className={`status-pill ${job.status}`}>
                            {isRunning ? "Running" : job.status}
                          </span>
                        </div>
                        <p title={job.source_path}>{shortPath(job.source_path)}</p>
                        <div className="job-meta">
                          <span>☁ {job.destination}</span>
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
                      <button
                        className="run-button"
                        disabled={isRunning}
                        onClick={() => void runJob(job)}
                      >
                        {isRunning ? "Backing up…" : "Back up now"}
                      </button>
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
        <div className="modal-backdrop" onMouseDown={() => setShowCreate(false)}>
          <section
            className="modal"
            role="dialog"
            aria-modal="true"
            aria-labelledby="create-title"
            onMouseDown={(event) => event.stopPropagation()}
          >
            <button className="modal-close" onClick={() => setShowCreate(false)}>
              ×
            </button>
            <p className="eyebrow">New scheduled backup</p>
            <h2 id="create-title">Protect something important</h2>
            <p className="modal-intro">
              CloudFolder uploads new and changed files. It will not delete existing
              cloud files.
            </p>

            <form onSubmit={createJob}>
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
                <div className="source-picker">
                  <div>
                    <strong>
                      {draft.source_path
                        ? shortPath(draft.source_path)
                        : "No source selected"}
                    </strong>
                    <small>Choose one file or an entire folder</small>
                  </div>
                  <button type="button" onClick={() => void chooseSource(false)}>
                    Choose file
                  </button>
                  <button type="button" onClick={() => void chooseSource(true)}>
                    Choose folder
                  </button>
                </div>
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

              <label>
                Run automatically
                <select
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
              </label>

              <div className="modal-actions">
                <button
                  type="button"
                  className="secondary"
                  onClick={() => setShowCreate(false)}
                >
                  Cancel
                </button>
                <button
                  className="primary"
                  disabled={
                    saving ||
                    !draft.source_path ||
                    !draft.remote ||
                    draft.cloud_path === null ||
                    !draft.name.trim()
                  }
                >
                  {saving ? "Creating…" : "Create backup"}
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
                <dt>Source</dt>
                <dd>{selectedJob.source_path}</dd>
              </div>
              <div>
                <dt>Destination</dt>
                <dd>{selectedJob.destination}</dd>
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
            <div className="danger-row">
              <button className="danger" onClick={() => void removeJob(selectedJob)}>
                Remove backup job
              </button>
              <small>Files already in the cloud will remain there.</small>
            </div>
          </section>
        </div>
      )}
    </div>
  );
}
