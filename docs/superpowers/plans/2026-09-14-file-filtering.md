# File Filtering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add 25 MiB source warnings plus app-wide and per-backup maximum-size and extension skip filters.

**Architecture:** SQLite stores a singleton global filter configuration and explicit inherit/custom/disabled modes on every job. Rust validates and resolves effective filters, scans local sources, and builds rclone arguments; the existing React screen exposes global and per-job controls and renders scan warnings.

**Tech Stack:** Rust, Tauri v2, rusqlite, serde/serde_json, React 19, TypeScript, Vite, rclone.

## Global Constraints

- Preserve the user's existing `formatFileSize` change in `src/App.tsx`.
- Existing jobs migrate to `inherit` for both filter types.
- The warning threshold is exactly 25 MiB (`25 * 1024 * 1024` bytes) and never silently enables skipping.
- Directory scans do not follow symlinks and return bounded example/error lists.
- Store extensions without a leading dot, normalized to lowercase.

---

### Task 1: Persist and resolve typed filter configuration

**Status:** Complete

**Files:**
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces: `AppSettings { max_file_size_mib: Option<i64>, excluded_extensions: Vec<String> }`.
- Produces: job fields `size_filter_mode`, `max_file_size_mib`, `extension_filter_mode`, and `excluded_extensions`.
- Produces: `resolve_job_filters(job: &SyncJob, settings: &AppSettings) -> EffectiveFilters`.

- [ ] **Step 1: Write failing Rust tests**

Add tests that initialize a legacy jobs table, run `initialize_database`, and assert the new job columns and singleton `app_settings` defaults exist. Add table-driven tests proving `inherit`, `custom`, and `disabled` precedence for size and extensions, plus validation failures for zero/negative sizes, invalid modes, duplicate extensions, and unsafe extension characters.

```rust
assert_eq!(
    resolve_job_filters(&inheriting_job(), &settings).max_file_size_mib,
    Some(100)
);
assert_eq!(
    resolve_job_filters(&disabled_job(), &settings).max_file_size_mib,
    None
);
assert!(validate_extension("tar.gz").is_err());
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml filter -- --nocapture`

Expected: compilation/test failure because the new types, fields, migrations, and resolver do not exist.

- [ ] **Step 3: Implement schema, models, validation, and persistence**

Create `app_settings` with id constrained to `1`, nullable `max_file_size_mib`, and JSON `excluded_extensions`. Add job columns with additive `ensure_job_column` migrations. Update every job SELECT/INSERT/UPDATE and `map_job`. Add Tauri commands:

```rust
#[tauri::command]
fn get_app_settings(state: State<'_, AppState>) -> AppResult<AppSettings>;

#[tauri::command]
fn update_app_settings(
    input: AppSettingsInput,
    state: State<'_, AppState>,
) -> AppResult<AppSettings>;
```

Normalize extensions by trimming whitespace and one leading dot, lowercasing, deduplicating, and accepting only ASCII alphanumeric characters plus `+`, `-`, and `_`. Limit lists to 100 entries and each extension to 32 characters. Accept size limits from 1 through 8,388,608 MiB. Reset `last_full_at` when any job filter field changes.

- [ ] **Step 4: Run focused and complete Rust tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml filter -- --nocapture`

Expected: filter tests pass.

Run: `cargo test --manifest-path src-tauri/Cargo.toml`

Expected: all existing and new Rust tests pass.

---

### Task 2: Scan selected sources for files over 25 MiB

**Status:** Complete

**Files:**
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces: `scan_large_files(input: LargeFileScanInput, state: State<'_, AppState>) -> AppResult<LargeFileScanResult>` Tauri command.
- Produces: `LargeFileScanResult { threshold_bytes, matching_file_count, examples, unreadable_path_count, unreadable_examples }`.

- [ ] **Step 1: Write failing scanner tests**

Create temporary nested files around the threshold using `File::set_len`, plus a directory symlink. Assert that exactly-over-threshold files are counted, examples are sorted largest-first and bounded, files at exactly 25 MiB are not counted, and the scanner does not traverse the symlink.

```rust
assert_eq!(result.threshold_bytes, 25 * 1024 * 1024);
assert_eq!(result.matching_file_count, 2);
assert!(result.examples[0].size_bytes >= result.examples[1].size_bytes);
```

- [ ] **Step 2: Run scanner tests and verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml large_file_scan -- --nocapture`

Expected: compilation/test failure because scanner types and functions do not exist.

- [ ] **Step 3: Implement bounded recursive scanning**

Use `symlink_metadata`, inspect regular files, walk directories iteratively, and never enqueue symlink directories. Keep total counts while retaining only the five largest matching files and five unreadable path examples. Register `scan_large_files` in `tauri::generate_handler!`.

- [ ] **Step 4: Run scanner and full Rust tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml large_file_scan -- --nocapture`

Expected: scanner tests pass.

Run: `cargo test --manifest-path src-tauri/Cargo.toml`

Expected: all Rust tests pass.

---

### Task 3: Apply effective filters to every rclone sync

**Status:** Complete

**Files:**
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Consumes: `resolve_job_filters` from Task 1.
- Produces: `append_filter_arguments(command: &mut Command, filters: &EffectiveFilters, custom_patterns: &[String])`.

- [ ] **Step 1: Write failing argument-generation tests**

Use a helper that exposes command arguments for assertions. Verify a 100 MiB limit generates `--max-size 100M`, normalized extensions generate extension exclusion rules, existing ignore rules remain present, and disabled/empty effective settings add no new arguments.

```rust
assert!(args.windows(2).any(|pair| pair == ["--max-size", "100M"]));
assert!(args.windows(2).any(|pair| pair == ["--exclude", "*.zip"]));
```

- [ ] **Step 2: Run rclone-argument tests and verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml rclone_filter_arguments -- --nocapture`

Expected: compilation/test failure because the helper does not exist.

- [ ] **Step 3: Resolve settings at run time and append arguments**

Load app settings before spawning the blocking backup worker, resolve the job's effective values, and pass them into `perform_copy`. Add `--max-size <N>M` and one extension rule per effective extension alongside current `--exclude` rules. Log a concise preparing activity when size or extension filters are active.

- [ ] **Step 4: Run focused and complete Rust tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml rclone_filter_arguments -- --nocapture`

Expected: argument tests pass.

Run: `cargo test --manifest-path src-tauri/Cargo.toml`

Expected: all Rust tests pass.

---

### Task 4: Add global settings, per-backup controls, and warnings

**Status:** Complete

**Files:**
- Modify: `src/App.tsx`
- Modify: `src/styles.css`

**Interfaces:**
- Consumes Tauri commands `get_app_settings`, `update_app_settings`, and `scan_large_files`.
- Sends the four new filter fields when creating/updating jobs.

- [ ] **Step 1: Extend TypeScript contracts and state**

Add `FilterMode`, `AppSettings`, `LargeFileScanResult`, and the job/draft filter fields. Fetch app settings during `refresh`, populate inherited defaults for new jobs, and preserve stored values when editing.

- [ ] **Step 2: Add the App filters modal**

Add a sidebar button and modal with no-limit/common-preset/custom maximum size controls and normalized extension chips. Save through `update_app_settings`, display backend validation errors, then refresh jobs/settings.

- [ ] **Step 3: Add per-backup filter controls**

Under the existing ignore fieldset, add independent inherit/custom/disabled choices for size and extensions. Show the currently inherited global value, validate required custom values before submit, and include fields in the create/update payload.

- [ ] **Step 4: Add the 25 MiB scan warning**

After `chooseSource` returns, invoke `scan_large_files` for the combined source list. Render a non-blocking amber warning with count, up to five `formatFileSize` examples, any partial-scan note, and a button that selects custom size mode at 25 MiB. Clear or refresh the warning when sources change.

- [ ] **Step 5: Add job-detail summaries and styles**

Show maximum-size and excluded-extension summaries in the selected job details. Add responsive modal, chip, segmented-control, warning, and scan-progress styles consistent with the existing visual language.

- [ ] **Step 6: Verify the frontend build**

Run: `npm run build`

Expected: TypeScript and Vite complete with exit code 0 and no errors.

---

### Task 5: Full regression verification

**Status:** Complete

**Files:**
- Verify: `src-tauri/src/lib.rs`
- Verify: `src/App.tsx`
- Verify: `src/styles.css`

- [ ] **Step 1: Run formatting checks**

Run: `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`

Expected: exit code 0.

- [ ] **Step 2: Run all automated checks**

Run: `cargo test --manifest-path src-tauri/Cargo.toml && npm run build`

Expected: all Rust tests pass and the production frontend build succeeds.

- [ ] **Step 3: Inspect the final diff**

Run: `git diff --check && git diff --stat && git status --short`

Expected: no whitespace errors; only the approved feature files, design/plan docs, and the user's pre-existing `formatFileSize` edit appear.
