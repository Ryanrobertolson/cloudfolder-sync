# File Filtering Design

## Goal

Warn users when selected backup sources contain files over 25 MiB, and let them skip files by maximum size or extension using either app-wide defaults or per-backup overrides.

## Configuration and precedence

The app stores global defaults in a singleton `app_settings` row:

- `max_file_size_mib`: optional positive whole number; `NULL` means no global limit.
- `excluded_extensions`: a JSON array of normalized extensions without leading dots.

Each backup stores two independent modes:

- `size_filter_mode`: `inherit`, `custom`, or `disabled`.
- `extension_filter_mode`: `inherit`, `custom`, or `disabled`.

A custom size requires `max_file_size_mib`; custom extensions use the backup's extension array. `inherit` resolves the current app setting at run time. `disabled` explicitly opts that backup out of the corresponding global filter. Existing backups migrate to `inherit` so later global changes apply predictably.

Values are validated in Rust. Size limits must be positive whole MiB values within SQLite and rclone-safe bounds. Extensions are trimmed, lowercased, deduplicated, limited in count and length, and restricted to letters, numbers, plus, minus, and underscores. A leading dot entered by the user is removed before validation.

## Sync execution

Immediately before a backup runs, the backend loads global settings and resolves the effective size and extension filters for that job. The effective size is passed to rclone with `--max-size`. Effective extensions become rclone exclude filters in addition to the existing free-form ignore rules.

Changing a job's filter mode or values invalidates its saved full/differential baseline just like changing an existing ignore rule, because the source set has changed.

The activity log reports how many filters are active without treating skipped files as errors.

## Large-file warning

A new Tauri preflight command recursively scans selected local files and folders for regular files larger than 25 MiB. It does not follow directory symlinks. The response contains the total count, a bounded list of the largest examples, and a bounded list/count of paths that could not be inspected.

The React form runs the scan after source selection and shows a non-blocking warning with the count and examples. The warning never silently enables a limit. It includes an action that switches the backup to a custom 25 MiB limit. A partial scan is clearly identified, while unreadable paths do not prevent saving the backup.

## User interface

The sidebar gains an App filters action and modal. It edits the global maximum file size and global excluded extensions.

The create/edit backup modal gains a File filters fieldset. Size and extension controls each support inheriting the app default, using a backup-specific value, or disabling the global setting. Size controls offer common presets and a positive whole-MiB custom field. Extensions are entered as removable chips.

Job details show the stored mode and effective summary so users can tell whether a rule comes from the app or that backup.

## Error handling

The backend rejects malformed modes, invalid sizes, and unsafe or excessive extension lists with user-facing validation errors. Database migrations use the existing additive migration pattern. Scan failures are returned as partial results when possible and as a command error only when no selected root can be inspected.

## Testing and verification

Rust tests cover:

- additive migrations and defaults for existing jobs;
- size and extension validation/normalization;
- global inheritance, custom overrides, and explicit disable behavior;
- large-file scanning, result ordering, and symlink handling;
- generated rclone size and extension arguments;
- create/update persistence and baseline invalidation.

Tests are written and observed failing before production changes. Final verification runs the full Rust test suite and the TypeScript production build. The unrelated existing `formatFileSize` edit in `src/App.tsx` is preserved.
