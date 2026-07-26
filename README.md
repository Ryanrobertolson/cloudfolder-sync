# CloudFolder Sync

[![Support Ryan on Ko-fi](https://ko-fi.com/img/githubbutton_sm.svg)](https://ko-fi.com/ryanrobertolson)

CloudFolder Sync is an Ubuntu desktop application for selecting local files or
folders and safely backing them up to Google Drive—or any cloud provider
supported by [rclone](https://rclone.org/).

The current MVP deliberately uses `rclone copy`, not `rclone sync`. It uploads
new and changed content without deleting files that already exist in the cloud.

## Current features

- Native Ubuntu file and folder picker
- Multiple local files and folders in a single backup job
- Google Drive folder browser with folder creation
- Persistent backup jobs stored in SQLite
- User-selectable schedules from every 15 minutes to daily
- Manual **Back up now** action
- Google Drive and other rclone remote discovery
- Background scheduling while the application is running
- System tray behavior when the main window is closed
- Recent run history and actionable transfer errors
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

Schedules run inside the CloudFolder process. Closing the main window hides it
to the system tray so schedules continue. Use **Quit CloudFolder** when you
actually want to stop the scheduler.

The next implementation milestone is a separate systemd user service so
scheduled backups can start automatically at login without first opening the
desktop interface.

## Safety boundaries

- Only paths readable by the current Ubuntu user can be backed up.
- Jobs never run as root.
- The MVP never automatically deletes cloud content.
- Removing a job only removes its local configuration and history.
- A disconnected drive or unavailable source produces an error instead of
  modifying the destination.
- When a job has multiple sources, each source is stored in its own named cloud
  subfolder to prevent filename collisions.
