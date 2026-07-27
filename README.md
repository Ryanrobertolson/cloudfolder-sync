# CloudFolder Sync

[![Support Ryan on Ko-fi](https://ko-fi.com/img/githubbutton_sm.svg)](https://ko-fi.com/ryanrobertolson)

CloudFolder Sync is an Ubuntu desktop application for selecting local files or
folders and safely backing them up to Google Drive—or any cloud provider
supported by [rclone](https://rclone.org/).

Incremental backup is the safe default and never deletes existing cloud files.
True mirroring is available as an explicitly destructive option with a warning.

## Current features

- Native Ubuntu file and folder picker
- Multiple local files and folders in a single backup job
- Google Drive folder browser with folder creation
- Persistent backup jobs stored in SQLite
- Simple schedule presets plus exact custom minute, hour, or day intervals
- Editable backup names, sources, destinations, and schedules
- Full, incremental, differential, and mirroring transfer modes
- Manual **Back up now** action
- Live percentage and progress bar while files are being copied
- Configurable cloud safety copies with in-app file recovery
- Google Drive and other rclone remote discovery
- Always-on scheduling through a systemd user service
- System tray behavior when the main window is closed
- Recent run history and a global, copyable error log
- Automatic GitHub release checks with package-aware update downloads
- Pause, resume, and remove jobs without deleting cloud data

## Prerequisites

- A recent Ubuntu installation
- `rclone`
- WebKitGTK and the standard Tauri Linux runtime dependencies

The development environment also needs Node.js, npm, Rust, and Cargo.

## Configure Google Drive

The normal setup does not require a terminal:

1. Open **Google Drive** in CloudFolder.
2. Press **Connect my Google Drive**.
3. Choose your Google account in the browser and press **Allow**.
4. Return to CloudFolder. It will show **Google Drive is ready!**

CloudFolder asks Google for Drive access so it can display your existing folder
names and place backups in the folder you select. CloudFolder never sees your
Google password, and the backup engine does not delete cloud files.

If the easy setup cannot open a browser, use **Advanced setup for grown-ups**.
You can also run the same advanced helper manually:

```bash
rclone config
```

Choose **New remote**, give it a short name such as `gdrive`, select Google
Drive, and complete the browser authorization flow. Verify it with:

```bash
rclone listremotes
rclone lsd gdrive:
```

CloudFolder reads the resulting remote names but does not read or display the
stored credentials.

## Support CloudFolder

If CloudFolder is useful to you, you can support continued development on
[Ryan's Ko-fi page](https://ko-fi.com/ryanrobertolson). The same link is
available from the **Support CloudFolder** button in the app sidebar.

## Development

Install JavaScript dependencies:

```bash
npm install
```

Run the desktop application:

```bash
npm run tauri dev
```

Validate both layers:

```bash
npm run build
cd src-tauri && cargo test
```

Build an Ubuntu package:

```bash
npm run tauri build
```

## Scheduling behavior

On the first packaged-app launch, CloudFolder installs and enables a systemd
user service. The service starts at login, restarts if it fails, and keeps
scheduled jobs running after the window or desktop application is closed. If
systemd is unavailable, the app falls back to its built-in scheduler. A normal
user service pauses when the Ubuntu user logs out; enable systemd user lingering
separately if backups must run before login.

Choose a simple preset when creating a backup, or open **More schedule options**
to enter an exact interval in minutes, hours, or days. Open **Error log** from
the sidebar to review the latest 100 failed runs or copy a troubleshooting
report.

To change an existing job, click its card, choose **Edit backup**, make the
changes, and press **Save changes**. Its existing run history is kept.

Backup types behave as follows:

- **Full backup** creates a new dated folder containing everything on every run.
- **Incremental backup** updates one cloud folder with new and changed files and
  never deletes cloud-only files.
- **Differential backup** creates a dated full baseline on its first run, then
  dated folders containing files changed since that baseline.
- **Mirroring** makes the destination match the computer and can delete files
  from the selected cloud destination.

## Previous files and recovery

Incremental and mirror jobs can keep older cloud files when a backup replaces
or deletes them. This protection is enabled by default for five backup runs and
can be set from 1 to 50, or turned off, in the backup editor.

Safety copies are stored outside the live backup in
`CloudFolder Previous Files/Backup <job number>`. After the configured number
of backup runs with changes, the oldest safety-copy folder is removed
automatically. Full and differential jobs already create dated cloud folders.

To recover something, open the backup card and select **Previous files**. Pick
a backup run and file, press **Restore**, and choose a folder on the computer.
CloudFolder never overwrites a local file during recovery; if the filename
already exists, the restored copy receives a new name.

## Program updates

CloudFolder checks the repository's latest published GitHub Release shortly
after startup. Use **Updates** in the sidebar to check manually, read release
notes, and download the correct Linux package.

- `.deb` installations download the new Ubuntu package and open the graphical
  package installer for password-confirmed installation.
- AppImage installations download an executable AppImage to `~/Downloads` and
  open that folder.

Updates are never silently installed, and download links are accepted only from
the project's GitHub Releases.

## Automatic GitHub releases

The **Release when version changes** GitHub Actions workflow watches the version
files on `main`. When the version changes, it verifies that `package.json`,
`package-lock.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`, and
`src-tauri/tauri.conf.json` all contain the same version. It then:

1. Builds and tests the application on Ubuntu.
2. Creates a `v<version>` tag on the exact commit that passed.
3. Generates release notes from the commits since the previous release.
4. Publishes a GitHub Release with the `.deb` and AppImage attached.

If the version did not change or that release already exists, the workflow
stops without publishing a duplicate. It can also be run manually from the
repository's **Actions** tab to publish the current version if its release is
missing.

## Safety boundaries

- Only paths readable by the current Ubuntu user can be backed up.
- Jobs never run as root.
- Full, incremental, and differential modes never delete cloud content.
- Mirroring can delete destination files, and requires an explicit warning
  acknowledgment before a job can be saved.
- Removing a job only removes its local configuration and history.
- A disconnected drive or unavailable source produces an error instead of
  modifying the destination.
- When a job has multiple sources, each source is stored in its own named cloud
  subfolder to prevent filename collisions.
