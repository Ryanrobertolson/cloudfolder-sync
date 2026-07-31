# Multi-cloud provider authentication — design

**Date:** 2026-07-30
**Status:** Approved, ready for implementation planning
**Component:** CloudFolder Sync (Tauri 2 + React)

## Problem

CloudFolder authenticates one cloud service: Google Drive. The rclone remote name
`CloudFolder:` is hardcoded, the connect command is `connect_google_drive`, and
roughly two dozen user-facing strings name Google specifically. Because the whole
transfer engine is rclone, every other backend rclone supports is already reachable
in principle — but only through the Advanced setup escape hatch that drops the user
into a terminal running `rclone config`.

This design extends first-class authentication to twelve more providers and removes
the Google-specific assumptions that block them.

## What rclone actually does

Measured against rclone v1.74.3 (`/usr/bin/rclone`) with a throwaway `RCLONE_CONFIG`
file. `rclone config create <name> <type> --non-interactive` reports the first
question the backend's config state machine would ask:

| Backend class | Backends | First state | Meaning |
|---|---|---|---|
| OAuth | `drive`, `dropbox`, `onedrive`, `box`, `pcloud`, `yandex` | `*oauth-islocal` | One question, `config_is_local`, default `true`. Without `--non-interactive` rclone takes the default and runs the local browser flow. |
| Credential | `b2`, `s3`, `mega`, `webdav`, `sftp`, `protondrive`, `koofr` | `""` (empty) | No questions at all. The remote is written from the `key=value` pairs in a single call. |
| Token-paste | `jottacloud` | `auth_type_done` | Asks `config_type`, then a single-use token the user generates on Jottacloud's website. No default; blocks on stdin. |

Three consequences drive the design:

1. Credential backends are **simpler** than OAuth, not harder. One non-interactive
   call creates the remote. There is no form state machine to drive.
2. The existing Google Drive code path — plain `rclone config create` with defaults
   auto-taken — generalizes unchanged to every OAuth backend. OneDrive's post-auth
   drive selection resolves through rclone's own defaults; no Microsoft Graph call
   is needed.
3. Jottacloud would hang the app. It is excluded.

Two supporting facts:

- `rclone listremotes --json` returns `[{name, type, source, description}]` — remote
  types without reading any credential.
- `rclone config providers` returns each backend's full option schema (`Required`,
  `IsPassword`, `Sensitive`, `Type`, `Examples`).

## Approach

A **curated provider catalog**: a hand-written table naming thirteen providers, each
mapped to its rclone backend and a short list of fields. Two code paths — browser
sign-in and details form.

The alternative of generating forms from `rclone config providers` was rejected on
the evidence: `s3` exposes 51 `provider` examples, 150 `region` values and 351
`endpoint` values, and `koofr` returns the option `password` three times as
provider-conditional variants. A generated form would be unusable, and would
contradict an interface deliberately written for non-technical users ("kid-steps",
"Advanced setup for grown-ups").

Driving rclone's `--state`/`--result` state machine generically was also rejected: it
reaches every backend including Jottacloud, but requires a general question-renderer
in React for a state machine whose questions change between rclone versions. That is
a large surface for marginal reach, and `open_rclone_config()` already covers the
long tail.

## Provider catalog

### Browser sign-in

All six pass the fixed pair `config_is_local=true`.

| Provider | rclone backend | Extra fixed values |
|---|---|---|
| Google Drive | `drive` | `scope=drive` |
| Dropbox | `dropbox` | — |
| Microsoft OneDrive | `onedrive` | — |
| Box | `box` | — |
| pCloud | `pcloud` | — |
| Yandex Disk | `yandex` | — |

### Details form

| Provider | rclone backend | Fields (`*` = required, `!` = secret) |
|---|---|---|
| Backblaze B2 | `b2` | `account`\* (Key ID), `key`\*! (Application Key) |
| S3-compatible | `s3` | `provider`\* (choice: AWS, Wasabi, Cloudflare R2, DigitalOcean, Minio, Other), `access_key_id`\*!, `secret_access_key`\*!, `region`, `endpoint` |
| Nextcloud / WebDAV | `webdav` | `url`\*, `vendor` (choice: Nextcloud, ownCloud, Other; default Other), `user`\*, `pass`\*! |
| SFTP / SSH | `sftp` | `host`\*, `user`\*, `port` (default 22), `pass`!, `key_file` (see note) |
| Mega | `mega` | `user`\*, `pass`\*!, `2fa` |
| Proton Drive | `protondrive` | `username`\*, `password`\*!, `2fa` |
| Koofr | `koofr` | `provider`\* (choice: Koofr, Digi Storage, Other), `user`\*, `password`\*! |

Two field-level rules that the table cannot express, stated here so they are not left
to interpretation:

- **SFTP needs one of `pass` or `key_file`.** Neither is individually required, but
  submitting both blank is a `Validation` error naming both fields. This is the only
  either-or rule in the catalog, and it is expressed as a per-provider validation hook
  rather than a general mechanism.
- **S3 `region` and `endpoint` are free text**, not dropdowns — rclone offers 150 and
  351 example values respectively, which no picker should reproduce. Selecting an S3
  `provider` prefills `endpoint` with that vendor's well-known value (Wasabi,
  Cloudflare R2, DigitalOcean) and leaves it blank for AWS, Minio and Other. The
  prefill is an editable default, never a locked value.

### Excluded

Jottacloud, and every other rclone backend. All remain reachable through Advanced
setup, which is unchanged.

## Architecture

### New module: `src-tauri/src/providers.rs`

`lib.rs` is 3338 lines. The catalog, its types, and the connect commands go in a new
module rather than growing it further. `lib.rs` keeps the backup engine, job storage,
scheduling and the existing cloud-browsing commands.

```rust
enum AuthKind { Browser, Fields }

struct FieldSpec {
    key: &'static str,          // rclone option name
    label: &'static str,        // user-facing
    help: &'static str,
    required: bool,
    secret: bool,               // render as password input, scrub from errors
    choices: &'static [(&'static str, &'static str)],  // (value, label)
    default: Option<&'static str>,
}

struct ProviderSpec {
    id: &'static str,           // "dropbox"
    label: &'static str,        // "Dropbox"
    backend: &'static str,      // rclone type
    auth: AuthKind,
    fixed: &'static [(&'static str, &'static str)],
    fields: &'static [FieldSpec],
}
```

### Commands

| Command | Signature | Behaviour |
|---|---|---|
| `list_providers` | `() -> Vec<ProviderInfo>` | Catalog projected for the UI: id, label, auth kind, field descriptors. Contains no credentials. |
| `connect_provider` | `(provider_id) -> String` | Browser sign-in. Same `rclone config create` shape as today's Google path. Returns the remote name. |
| `connect_provider_with_fields` | `(provider_id, fields: HashMap<String,String>) -> String` | Validates required fields, then one non-interactive `config create`. Returns the remote name. |
| `list_remotes` | `() -> Vec<RemoteInfo>` | `RemoteInfo { name, backend, label }` from `rclone listremotes --json`, labelled through the catalog. Replaces the current `Vec<String>`. |

`list_remotes` returns **every** configured remote, including ones the user created
through Advanced setup that the catalog knows nothing about. A remote whose `backend`
has no catalog entry gets `label` set to the backend id as reported by rclone (e.g.
`seafile`), so it still appears in the job editor and stays selectable. Unknown
backends are never hidden — hiding them would silently break jobs that already target
them.
| `disconnect_remote` | `(name) -> ()` | `rclone config delete`. Refuses if any saved job's destination targets the remote, naming the jobs. |
| `open_rclone_config` | unchanged | Advanced setup escape hatch. |

`connect_google_drive` is deleted. Its single caller in `App.tsx` moves to
`connect_provider("google_drive")`.

### Remote naming

`remote_name_for(provider, existing_remotes) -> String`:

- Google Drive resolves to `CloudFolder` — unchanged, so existing jobs whose
  destination starts with `CloudFolder:` keep working with no migration.
- Every other provider resolves to `CloudFolder-<Label>`, e.g. `CloudFolder-Dropbox`,
  `CloudFolder-B2`.
- If that name exists and its backend **matches**, reuse it. This re-runs auth
  against the same remote, matching today's Google Drive behaviour of refreshing an
  existing connection.
- If it exists and its backend **differs**, append `-2`, then `-3`, until free.

`cloudfolder_drive_has_browse_access` generalizes to
`remote_matches_backend(name, backend) -> AppResult<bool>`, still reading via
`rclone config redacted`.

### Credential handling

The existing privacy properties are preserved, and the README's promise — "CloudFolder
reads the resulting remote names but does not read or display the stored credentials"
— continues to hold.

- `rclone config show` is never invoked. Only `config redacted` and
  `listremotes --json`.
- Submitted field values pass UI → Tauri command → rclone argv. They are never
  persisted app-side, never written to the activity log, and never returned to the
  frontend.
- `compact_output` gains a scrub pass: any submitted secret value appearing in
  rclone's stdout/stderr is replaced with `***` before the error reaches the UI.
- Password fields reach rclone unobscured and rclone obscures them on write, which is
  its documented behaviour for `IsPassword` options.

**Accepted exposure, stated explicitly:** those values appear in `/proc/<pid>/cmdline`
for the lifetime of the rclone call, readable by the same user. `rclone config create`
has no stdin path for option values, so this is unavoidable within the chosen
approach. It is not a new exposure — `~/.config/rclone/rclone.conf` is already
user-readable and the app already runs unprivileged as that user — but it is a real
property of the design and is recorded here rather than left implicit.

### Verification on connect

After a credential remote is created, run `rclone lsd <remote>: --max-depth 1`. On
failure, delete the newly created remote and surface rclone's actual error text
(scrubbed). Bad keys fail at connect time, in front of the user, rather than silently
at the next scheduled backup.

Browser sign-in keeps the current post-auth check: confirm the remote appears in
`listremotes`.

## Frontend changes

### Types and state

`const [remotes, setRemotes] = useState<string[]>([])` becomes
`useState<RemoteInfo[]>([])`. Call sites that need a bare name use `r.name`. The job
editor's remote-derivation logic
(`remotes.find((remote) => job.destination.startsWith(remote))`) matches on `r.name`.
`JobDraft.remote` stays a `string` holding the remote name, so job persistence and
`destination` construction are unchanged.

### Cloud setup modal

A provider picker replaces the single "Connect my Google Drive" screen:

- Two groups, **Sign in with your browser** and **Enter your details**, each a grid of
  provider tiles.
- Already-connected accounts appear at the top with a disconnect control.
- Choosing a browser provider shows the existing three-step flow with the provider
  label substituted into the copy.
- Choosing a form provider renders a form generated from that provider's `FieldSpec`
  list — `choices` as a `<select>`, `secret` as `<input type="password">`, `help` as
  hint text — with a Connect button that calls `connect_provider_with_fields`.
- `configureCloud()` no longer tests `remotes.includes("CloudFolder:")`; it opens the
  picker.

The existing visual language (`kid-steps`, `giant-button`, `cloud-success`,
`cloud-waiting`, `advanced-link`) is reused. New styles are limited to the tile grid
and the field form.

### Copy

Google-specific strings generalize to the provider label or a neutral term:

| Location | Before | After |
|---|---|---|
| Sidebar nav | `☁ Google Drive` | `☁ Cloud accounts` |
| Empty state | "Connect Google Drive" | "Connect a cloud account" |
| Job editor label | "Google Drive folder" | "Cloud folder" |
| Folder browser | "Click to browse Google Drive" | "Click to browse {provider}" |
| Job editor hint | "Each item gets its own named folder in Google Drive" | "Each item gets its own named folder in the cloud" |
| Remote `<select>` | remote name only | `Dropbox — CloudFolder-Dropbox:` |

## Error handling

- Missing or blank required field → `AppError::Validation` before rclone is spawned,
  naming the field by its user-facing label.
- Unknown `provider_id` → `AppError::Validation`.
- Remote-name collision with a different backend is resolved by suffixing, not by
  erroring.
- `ensure_configured_remote`'s message changes from "Choose a connected Google Drive
  account first" to "Choose a connected cloud account first".
- `list_cloud_folders` and `create_cloud_folder` are already remote-agnostic in logic;
  only their strings change ("CloudFolder could not look inside {provider}: {error}").
- `disconnect_remote` on a remote in use → `AppError::Validation` listing the job
  names.

## Testing

**Rust unit tests** (no network, no rclone invocation):

- Catalog integrity: provider ids unique; every `backend` non-empty; every
  `AuthKind::Fields` provider has at least one required field; every
  `AuthKind::Browser` provider sets `config_is_local=true`; every `choices` list is
  non-empty, and where a `default` is present it is one of that list's values.
- SFTP's either-or rule: blank `pass` and blank `key_file` is rejected; either one
  alone is accepted.
- Unknown-backend labelling: a `RemoteInfo` built from a backend absent from the
  catalog falls back to the backend id rather than being dropped.
- Google Drive regression: the catalog entry resolves to backend `drive`, includes
  `scope=drive`, and yields remote name `CloudFolder`. This replaces the existing
  `google_drive_config_args` test.
- `remote_name_for` as a pure function over a supplied remote list: fresh name,
  same-backend reuse, different-backend suffixing, repeated suffixing.
- Required-field validation rejects missing keys and whitespace-only values.
- Secret scrubbing removes a submitted secret from a synthetic error string and
  leaves non-secret text intact.

**Frontend:** `npm run build` for type-checking across the `string[]` → `RemoteInfo[]`
change.

**Manual, end to end:** connect one browser provider (Dropbox) and one form provider
(Backblaze B2 or a WebDAV endpoint); browse folders on each; run one backup against a
non-Google remote; confirm an existing `CloudFolder:` job still runs untouched.

## Documentation

`README.md`: the "Configure Google Drive" section becomes "Connect a cloud account",
with a table of the thirteen supported providers and what each asks for. Feature
bullets generalize. The two credential-privacy statements are kept verbatim. Fallback
CLI snippets change from `rclone lsd gdrive:` to a provider-neutral example.

## Out of scope

- Per-provider brand icons or logo assets.
- Multi-account UI beyond automatic `-2` suffixing.
- `crypt` and `chunker` wrapper remotes.
- Any migration of existing jobs; `CloudFolder:` is preserved precisely so none is
  needed.
- Jottacloud and the remaining rclone backends, which stay on Advanced setup.
