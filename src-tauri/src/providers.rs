//! Cloud provider catalogue and connection commands.
//!
//! Everything CloudFolder knows about a specific cloud service lives here: which
//! rclone backend it maps to, whether signing in happens in a web browser or
//! through a form, and which fields that form asks for. The rest of the app stays
//! provider-agnostic and only ever handles remote names.
//!
//! Two classes of provider exist, matching how rclone's config state machine
//! behaves:
//!
//! * [`AuthKind::Browser`] backends stop at a single defaulted question and then
//!   run an OAuth flow in the user's browser.
//! * [`AuthKind::Fields`] backends ask nothing at all; the remote is written from
//!   the key/value pairs collected by the form.
//!
//! Anything not in this catalogue is still reachable through advanced setup,
//! which drops the user into `rclone config`.

use crate::{compact_output, AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Command;

/// How a provider proves who the user is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    /// rclone opens a browser and completes an OAuth flow.
    Browser,
    /// The user types credentials into a form.
    Fields,
}

/// One selectable value for a field rendered as a drop-down.
pub struct Choice {
    pub value: &'static str,
    pub label: &'static str,
    /// Other field values to prefill when this choice is picked. Always an
    /// editable suggestion, never a locked value.
    pub prefill: &'static [(&'static str, &'static str)],
}

/// One question on a provider's sign-in form, mapped to an rclone option.
pub struct FieldSpec {
    /// rclone option name, used verbatim as `key=value` on the command line.
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub required: bool,
    /// Rendered as a password input, and scrubbed out of any error text.
    pub secret: bool,
    pub choices: &'static [Choice],
    pub default: Option<&'static str>,
}

const fn text(
    key: &'static str,
    label: &'static str,
    help: &'static str,
    required: bool,
    secret: bool,
) -> FieldSpec {
    FieldSpec {
        key,
        label,
        help,
        required,
        secret,
        choices: &[],
        default: None,
    }
}

const fn text_with_default(
    key: &'static str,
    label: &'static str,
    help: &'static str,
    default: &'static str,
) -> FieldSpec {
    FieldSpec {
        key,
        label,
        help,
        required: false,
        secret: false,
        choices: &[],
        default: Some(default),
    }
}

const fn choice(
    key: &'static str,
    label: &'static str,
    help: &'static str,
    choices: &'static [Choice],
    default: &'static str,
) -> FieldSpec {
    FieldSpec {
        key,
        label,
        help,
        required: true,
        secret: false,
        choices,
        default: Some(default),
    }
}

/// One cloud service CloudFolder can connect to.
pub struct ProviderSpec {
    /// Stable identifier used by the frontend.
    pub id: &'static str,
    pub label: &'static str,
    /// rclone backend type.
    pub backend: &'static str,
    pub auth: AuthKind,
    pub blurb: &'static str,
    /// Appended to `CloudFolder-` to name the remote. Empty means the bare name
    /// `CloudFolder`, which Google Drive keeps for backwards compatibility.
    pub remote_suffix: &'static str,
    /// Options that must end up saved in the remote's config. Their presence is
    /// how an already-usable connection is recognised.
    pub stored_options: &'static [(&'static str, &'static str)],
    /// Answers fed to rclone's config flow. These drive the sign-in and are not
    /// written to the config file.
    pub config_answers: &'static [(&'static str, &'static str)],
    pub fields: &'static [FieldSpec],
    /// Field keys of which at least one must be filled in. Empty means no such
    /// rule applies.
    pub require_one_of: &'static [&'static str],
}

const BROWSER_ANSWERS: &[(&str, &str)] = &[("config_is_local", "true")];

const S3_PROVIDERS: &[Choice] = &[
    Choice {
        value: "AWS",
        label: "Amazon S3",
        prefill: &[("endpoint", "")],
    },
    Choice {
        value: "Wasabi",
        label: "Wasabi",
        prefill: &[("endpoint", "s3.wasabisys.com")],
    },
    Choice {
        value: "Cloudflare",
        label: "Cloudflare R2",
        prefill: &[("endpoint", "")],
    },
    Choice {
        value: "DigitalOcean",
        label: "DigitalOcean Spaces",
        prefill: &[("endpoint", "nyc3.digitaloceanspaces.com")],
    },
    Choice {
        value: "Minio",
        label: "Minio",
        prefill: &[("endpoint", "")],
    },
    Choice {
        value: "Other",
        label: "Something else",
        prefill: &[("endpoint", "")],
    },
];

const WEBDAV_VENDORS: &[Choice] = &[
    Choice {
        value: "nextcloud",
        label: "Nextcloud",
        prefill: &[],
    },
    Choice {
        value: "owncloud",
        label: "ownCloud",
        prefill: &[],
    },
    Choice {
        value: "other",
        label: "Other WebDAV server",
        prefill: &[],
    },
];

const KOOFR_PROVIDERS: &[Choice] = &[
    Choice {
        value: "koofr",
        label: "Koofr",
        prefill: &[("endpoint", "https://app.koofr.net/")],
    },
    Choice {
        value: "digistorage",
        label: "Digi Storage",
        prefill: &[("endpoint", "https://storage.rcs-rds.ro/")],
    },
    Choice {
        value: "other",
        label: "Other Koofr-compatible service",
        prefill: &[("endpoint", "")],
    },
];

/// The providers offered in the picker, in display order.
static CATALOG: &[ProviderSpec] = &[
    ProviderSpec {
        id: "google_drive",
        label: "Google Drive",
        backend: "drive",
        auth: AuthKind::Browser,
        blurb: "Your personal Google Drive",
        remote_suffix: "",
        stored_options: &[("scope", "drive")],
        config_answers: BROWSER_ANSWERS,
        fields: &[],
        require_one_of: &[],
    },
    ProviderSpec {
        id: "dropbox",
        label: "Dropbox",
        backend: "dropbox",
        auth: AuthKind::Browser,
        blurb: "Your Dropbox account",
        remote_suffix: "Dropbox",
        stored_options: &[],
        config_answers: BROWSER_ANSWERS,
        fields: &[],
        require_one_of: &[],
    },
    ProviderSpec {
        id: "onedrive",
        label: "Microsoft OneDrive",
        backend: "onedrive",
        auth: AuthKind::Browser,
        blurb: "OneDrive, personal or work",
        remote_suffix: "OneDrive",
        stored_options: &[],
        config_answers: BROWSER_ANSWERS,
        fields: &[],
        require_one_of: &[],
    },
    ProviderSpec {
        id: "box",
        label: "Box",
        backend: "box",
        auth: AuthKind::Browser,
        blurb: "Your Box account",
        remote_suffix: "Box",
        stored_options: &[],
        config_answers: BROWSER_ANSWERS,
        fields: &[],
        require_one_of: &[],
    },
    ProviderSpec {
        id: "pcloud",
        label: "pCloud",
        backend: "pcloud",
        auth: AuthKind::Browser,
        blurb: "Your pCloud storage",
        remote_suffix: "pCloud",
        stored_options: &[],
        config_answers: BROWSER_ANSWERS,
        fields: &[],
        require_one_of: &[],
    },
    ProviderSpec {
        id: "yandex",
        label: "Yandex Disk",
        backend: "yandex",
        auth: AuthKind::Browser,
        blurb: "Your Yandex Disk",
        remote_suffix: "Yandex",
        stored_options: &[],
        config_answers: BROWSER_ANSWERS,
        fields: &[],
        require_one_of: &[],
    },
    ProviderSpec {
        id: "b2",
        label: "Backblaze B2",
        backend: "b2",
        auth: AuthKind::Fields,
        blurb: "Cheap bucket storage",
        remote_suffix: "B2",
        stored_options: &[],
        config_answers: &[],
        fields: &[
            text(
                "account",
                "Key ID",
                "From the Application Keys page in your Backblaze account.",
                true,
                false,
            ),
            text(
                "key",
                "Application Key",
                "Shown once when you create the key. Copy it before closing the page.",
                true,
                true,
            ),
        ],
        require_one_of: &[],
    },
    ProviderSpec {
        id: "s3",
        label: "S3 storage",
        backend: "s3",
        auth: AuthKind::Fields,
        blurb: "Amazon S3, Wasabi, R2 and friends",
        remote_suffix: "S3",
        stored_options: &[],
        config_answers: &[],
        fields: &[
            choice(
                "provider",
                "Service",
                "Pick the company that hosts your storage.",
                S3_PROVIDERS,
                "AWS",
            ),
            text(
                "access_key_id",
                "Access key ID",
                "The shorter of the two keys you were given.",
                true,
                false,
            ),
            text(
                "secret_access_key",
                "Secret access key",
                "The longer, secret key. Keep it private.",
                true,
                true,
            ),
            text(
                "region",
                "Region",
                "Optional. For example us-east-1.",
                false,
                false,
            ),
            text(
                "endpoint",
                "Endpoint",
                "Optional. Leave blank unless your service gave you an address.",
                false,
                false,
            ),
        ],
        require_one_of: &[],
    },
    ProviderSpec {
        id: "webdav",
        label: "Nextcloud or WebDAV",
        backend: "webdav",
        auth: AuthKind::Fields,
        blurb: "Nextcloud, ownCloud, or any WebDAV server",
        remote_suffix: "WebDAV",
        stored_options: &[],
        config_answers: &[],
        fields: &[
            text(
                "url",
                "Server address",
                "For Nextcloud this ends in /remote.php/dav/files/yourname/",
                true,
                false,
            ),
            choice(
                "vendor",
                "Server software",
                "Pick Other if you are not sure.",
                WEBDAV_VENDORS,
                "other",
            ),
            text("user", "Username", "Your login name on that server.", true, false),
            text(
                "pass",
                "Password",
                "Use an app password if your server offers one.",
                true,
                true,
            ),
        ],
        require_one_of: &[],
    },
    ProviderSpec {
        id: "sftp",
        label: "SFTP server",
        backend: "sftp",
        auth: AuthKind::Fields,
        blurb: "Any computer you can reach over SSH",
        remote_suffix: "SFTP",
        stored_options: &[],
        config_answers: &[],
        fields: &[
            text(
                "host",
                "Server address",
                "A name or IP address, for example backup.example.com",
                true,
                false,
            ),
            text("user", "Username", "Your login name on that server.", true, false),
            text_with_default("port", "Port", "Leave as 22 unless told otherwise.", "22"),
            text(
                "pass",
                "Password",
                "Fill this in, or use a key file below.",
                false,
                true,
            ),
            text(
                "key_file",
                "Key file",
                "Full path to a private key, for example /home/you/.ssh/id_ed25519",
                false,
                false,
            ),
        ],
        require_one_of: &["pass", "key_file"],
    },
    ProviderSpec {
        id: "mega",
        label: "Mega",
        backend: "mega",
        auth: AuthKind::Fields,
        blurb: "Your Mega account",
        remote_suffix: "Mega",
        stored_options: &[],
        config_answers: &[],
        fields: &[
            text("user", "Email address", "The email you sign in with.", true, false),
            text("pass", "Password", "Your Mega password.", true, true),
            text(
                "2fa",
                "Two-factor code",
                "Only if your account asks for one.",
                false,
                true,
            ),
        ],
        require_one_of: &[],
    },
    ProviderSpec {
        id: "protondrive",
        label: "Proton Drive",
        backend: "protondrive",
        auth: AuthKind::Fields,
        blurb: "Your Proton Drive",
        remote_suffix: "Proton",
        stored_options: &[],
        config_answers: &[],
        fields: &[
            text(
                "username",
                "Email address",
                "The email you sign in with.",
                true,
                false,
            ),
            text("password", "Password", "Your Proton password.", true, true),
            text(
                "2fa",
                "Two-factor code",
                "Only if your account asks for one.",
                false,
                true,
            ),
        ],
        require_one_of: &[],
    },
    ProviderSpec {
        id: "koofr",
        label: "Koofr",
        backend: "koofr",
        auth: AuthKind::Fields,
        blurb: "Koofr or Digi Storage",
        remote_suffix: "Koofr",
        stored_options: &[],
        config_answers: &[],
        fields: &[
            choice(
                "provider",
                "Service",
                "Pick the company that hosts your storage.",
                KOOFR_PROVIDERS,
                "koofr",
            ),
            text(
                "endpoint",
                "Server address",
                "Filled in for you unless you picked something else.",
                true,
                false,
            ),
            text("user", "Username", "Your login name.", true, false),
            text(
                "password",
                "App password",
                "Generate this in your Koofr account settings, not your login password.",
                true,
                true,
            ),
        ],
        require_one_of: &[],
    },
];

/// A provider as sent to the frontend. Carries no credentials.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderInfo {
    pub id: String,
    pub label: String,
    pub backend: String,
    pub auth: AuthKind,
    pub blurb: String,
    pub fields: Vec<FieldInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldInfo {
    pub key: String,
    pub label: String,
    pub help: String,
    pub required: bool,
    pub secret: bool,
    pub default: String,
    pub choices: Vec<ChoiceInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChoiceInfo {
    pub value: String,
    pub label: String,
    pub prefill: Vec<(String, String)>,
}

/// A configured rclone remote. `name` keeps its trailing colon so it can be
/// concatenated with a cloud path directly.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteInfo {
    pub name: String,
    pub backend: String,
    pub label: String,
}

#[cfg(test)]
pub fn catalog() -> &'static [ProviderSpec] {
    CATALOG
}

fn find_provider(id: &str) -> AppResult<&'static ProviderSpec> {
    CATALOG
        .iter()
        .find(|provider| provider.id == id)
        .ok_or_else(|| AppError::Validation(format!("Unknown cloud service: {id}")))
}

fn provider_for_backend(backend: &str) -> Option<&'static ProviderSpec> {
    CATALOG
        .iter()
        .find(|provider| provider.backend == backend)
}

/// Friendly name for a remote, falling back to the rclone backend id for
/// remotes created outside CloudFolder.
fn label_for(backend: &str, name: &str) -> String {
    if let Some(provider) = provider_for_backend(backend) {
        return provider.label.to_owned();
    }
    if backend.is_empty() {
        name.trim_end_matches(':').to_owned()
    } else {
        backend.to_owned()
    }
}

fn bare(name: &str) -> &str {
    name.trim_end_matches(':')
}

/// Picks the remote name a provider should use.
///
/// Google Drive keeps the bare `CloudFolder` name so jobs created before
/// multi-provider support keep working untouched. A name already taken by the
/// same backend is reused, which re-runs sign-in against the existing remote. A
/// name taken by a different backend is suffixed until a free or matching one is
/// found.
pub fn remote_name_for(provider: &ProviderSpec, existing: &[RemoteInfo]) -> String {
    let base = if provider.remote_suffix.is_empty() {
        "CloudFolder".to_owned()
    } else {
        format!("CloudFolder-{}", provider.remote_suffix)
    };

    let taken_by_other = |candidate: &str| {
        existing
            .iter()
            .any(|remote| bare(&remote.name) == candidate && remote.backend != provider.backend)
    };

    if !taken_by_other(&base) {
        return base;
    }
    let mut counter = 2;
    loop {
        let candidate = format!("{base}-{counter}");
        if !taken_by_other(&candidate) {
            return candidate;
        }
        counter += 1;
    }
}

/// Resolves the values to hand rclone, applying defaults and checking the
/// provider's own requirements before anything is spawned.
pub fn resolve_values(
    provider: &ProviderSpec,
    submitted: &HashMap<String, String>,
) -> AppResult<Vec<(String, String)>> {
    let mut resolved: Vec<(String, String)> = Vec::new();
    for field in provider.fields {
        let value = submitted
            .get(field.key)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .or_else(|| field.default.map(str::to_owned));

        match value {
            Some(value) => {
                if !field.choices.is_empty()
                    && !field
                        .choices
                        .iter()
                        .any(|choice| choice.value == value)
                {
                    return Err(AppError::Validation(format!(
                        "Choose one of the offered options for {}",
                        field.label
                    )));
                }
                resolved.push((field.key.to_owned(), value));
            }
            None if field.required => {
                return Err(AppError::Validation(format!("Enter the {}", field.label)));
            }
            None => {}
        }
    }

    if !provider.require_one_of.is_empty() {
        let satisfied = provider
            .require_one_of
            .iter()
            .any(|key| resolved.iter().any(|(name, _)| name == key));
        if !satisfied {
            let labels: Vec<&str> = provider
                .require_one_of
                .iter()
                .filter_map(|key| {
                    provider
                        .fields
                        .iter()
                        .find(|field| &field.key == key)
                        .map(|field| field.label)
                })
                .collect();
            return Err(AppError::Validation(format!(
                "Fill in one of these: {}",
                labels.join(" or ")
            )));
        }
    }

    Ok(resolved)
}

/// Replaces submitted secrets with `***` so they cannot reach the interface
/// through an error message. Very short values are left alone; they would match
/// too much unrelated text to be worth redacting.
pub fn scrub_secrets(text: &str, secrets: &[String]) -> String {
    let mut cleaned = text.to_owned();
    for secret in secrets {
        if secret.chars().count() < 3 {
            continue;
        }
        cleaned = cleaned.replace(secret.as_str(), "***");
    }
    cleaned
}

fn secret_values(provider: &ProviderSpec, values: &[(String, String)]) -> Vec<String> {
    values
        .iter()
        .filter(|(key, _)| {
            provider
                .fields
                .iter()
                .any(|field| field.key == key && field.secret)
        })
        .map(|(_, value)| value.clone())
        .collect()
}

#[derive(Deserialize)]
struct RcloneRemote {
    name: String,
    #[serde(rename = "type")]
    backend: String,
}

/// Every configured remote, including ones made outside CloudFolder. Reads names
/// and backend types only; credentials are never touched.
pub fn remote_list() -> AppResult<Vec<RemoteInfo>> {
    let output = Command::new("rclone")
        .args(["listremotes", "--json"])
        .output()
        .map_err(|error| {
            AppError::Transfer(format!(
                "Could not run rclone: {error}. Install rclone and try again."
            ))
        })?;

    if output.status.success() {
        if let Ok(parsed) = serde_json::from_slice::<Vec<RcloneRemote>>(&output.stdout) {
            return Ok(parsed
                .into_iter()
                .map(|remote| {
                    let name = format!("{}:", bare(&remote.name));
                    let label = label_for(&remote.backend, &name);
                    RemoteInfo {
                        name,
                        backend: remote.backend,
                        label,
                    }
                })
                .collect());
        }
    }

    // Older rclone builds have no --json on listremotes. Names still work; the
    // remotes just show up without a friendly provider label.
    let plain = Command::new("rclone").arg("listremotes").output()?;
    if !plain.status.success() {
        return Err(AppError::Transfer(compact_output(
            &plain.stdout,
            &plain.stderr,
        )));
    }
    Ok(String::from_utf8_lossy(&plain.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| RemoteInfo {
            name: line.to_owned(),
            backend: String::new(),
            label: bare(line).to_owned(),
        })
        .collect())
}

/// Whether a remote already stores every option a provider needs. Uses
/// `config redacted`, never `config show`, so stored secrets stay unread.
fn remote_has_options(name: &str, options: &[(&str, &str)]) -> AppResult<bool> {
    if options.is_empty() {
        return Ok(true);
    }
    let output = Command::new("rclone")
        .args(["config", "redacted", name])
        .output()?;
    if !output.status.success() {
        return Ok(false);
    }
    let config = String::from_utf8_lossy(&output.stdout);
    Ok(options.iter().all(|(key, value)| {
        let expected = format!("{key} = {value}");
        config
            .lines()
            .any(|line| line.trim().eq_ignore_ascii_case(&expected))
    }))
}

fn remote_exists(existing: &[RemoteInfo], name: &str) -> bool {
    existing.iter().any(|remote| bare(&remote.name) == name)
}

fn delete_quietly(name: &str) {
    let _ = Command::new("rclone")
        .args(["config", "delete", name])
        .output();
}

/// Checks a freshly configured remote can actually be reached. Bad credentials
/// then fail in front of the user rather than silently at the next backup.
fn verify_remote(name: &str) -> AppResult<()> {
    let output = Command::new("rclone")
        .arg("lsd")
        .arg(format!("{name}:"))
        .args([
            "--max-depth",
            "1",
            "--contimeout",
            "15s",
            "--timeout",
            "30s",
            "--retries",
            "1",
            "--low-level-retries",
            "1",
        ])
        .output()
        .map_err(|error| AppError::Transfer(format!("Could not run rclone: {error}")))?;
    if output.status.success() {
        return Ok(());
    }
    Err(AppError::Transfer(compact_output(
        &output.stdout,
        &output.stderr,
    )))
}

fn run_config_command(
    name: &str,
    backend: &str,
    update: bool,
    values: &[(String, String)],
    extra_flags: &[&str],
) -> AppResult<std::process::Output> {
    let mut command = Command::new("rclone");
    command.arg("config");
    if update {
        command.arg("update").arg(name);
    } else {
        command.arg("create").arg(name).arg(backend);
    }
    // `key=value` in a single argument, so a value can never be mistaken for a
    // command-line flag.
    for (key, value) in values {
        command.arg(format!("{key}={value}"));
    }
    if update {
        command.arg("--auto-confirm");
    }
    command.args(extra_flags);
    command
        .output()
        .map_err(|error| AppError::Transfer(format!("Could not run rclone: {error}")))
}

/// Signs in to a browser-based provider. Returns the remote name, colon included.
pub fn connect_browser(id: &str) -> AppResult<String> {
    let provider = find_provider(id)?;
    if provider.auth != AuthKind::Browser {
        return Err(AppError::Validation(format!(
            "{} does not sign in through a browser",
            provider.label
        )));
    }

    let existing = remote_list()?;
    let name = remote_name_for(provider, &existing);
    let already_there = remote_exists(&existing, &name);

    if already_there && remote_has_options(&name, provider.stored_options)? {
        return Ok(format!("{name}:"));
    }

    let mut values: Vec<(String, String)> = Vec::new();
    for (key, value) in provider
        .stored_options
        .iter()
        .chain(provider.config_answers)
    {
        values.push(((*key).to_owned(), (*value).to_owned()));
    }

    let output = run_config_command(&name, provider.backend, already_there, &values, &[])?;
    if !output.status.success() {
        let details = compact_output(&output.stdout, &output.stderr);
        return Err(AppError::Transfer(format!(
            "{} did not finish connecting. {details}",
            provider.label
        )));
    }

    if remote_exists(&remote_list()?, &name) {
        Ok(format!("{name}:"))
    } else {
        Err(AppError::Transfer(format!(
            "{} sign-in finished, but the connection was not saved. Try again.",
            provider.label
        )))
    }
}

/// Creates a credential-based remote from submitted form values. Returns the
/// remote name, colon included.
pub fn connect_fields(id: &str, submitted: HashMap<String, String>) -> AppResult<String> {
    let provider = find_provider(id)?;
    if provider.auth != AuthKind::Fields {
        return Err(AppError::Validation(format!(
            "{} signs in through a browser instead",
            provider.label
        )));
    }

    let values = resolve_values(provider, &submitted)?;
    let secrets = secret_values(provider, &values);

    let existing = remote_list()?;
    let name = remote_name_for(provider, &existing);
    let already_there = remote_exists(&existing, &name);

    let output = run_config_command(
        &name,
        provider.backend,
        already_there,
        &values,
        // --non-interactive guarantees rclone can never block waiting on stdin.
        // --obscure makes certain any password field is stored obscured.
        &["--non-interactive", "--obscure"],
    )?;

    if !output.status.success() {
        let details = scrub_secrets(&compact_output(&output.stdout, &output.stderr), &secrets);
        if !already_there {
            delete_quietly(&name);
        }
        return Err(AppError::Transfer(format!(
            "{} could not be set up. {details}",
            provider.label
        )));
    }

    if let Err(error) = verify_remote(&name) {
        // Only clean up remotes this call created. Removing one the user already
        // had would destroy configuration we were only meant to update.
        if !already_there {
            delete_quietly(&name);
        }
        let details = scrub_secrets(&error.to_string(), &secrets);
        return Err(AppError::Transfer(format!(
            "{} refused those details. {details}",
            provider.label
        )));
    }

    Ok(format!("{name}:"))
}

/// Forgets a remote. The caller is responsible for checking no job needs it.
pub fn delete_remote(name: &str) -> AppResult<()> {
    let trimmed = bare(name.trim());
    if trimmed.is_empty() {
        return Err(AppError::Validation("Choose a cloud account first".into()));
    }
    let output = Command::new("rclone")
        .args(["config", "delete", trimmed])
        .output()
        .map_err(|error| AppError::Transfer(format!("Could not run rclone: {error}")))?;
    if !output.status.success() {
        return Err(AppError::Transfer(compact_output(
            &output.stdout,
            &output.stderr,
        )));
    }
    Ok(())
}

/// The catalogue as the interface needs it.
pub fn provider_infos() -> Vec<ProviderInfo> {
    CATALOG
        .iter()
        .map(|provider| ProviderInfo {
            id: provider.id.to_owned(),
            label: provider.label.to_owned(),
            backend: provider.backend.to_owned(),
            auth: provider.auth,
            blurb: provider.blurb.to_owned(),
            fields: provider
                .fields
                .iter()
                .map(|field| FieldInfo {
                    key: field.key.to_owned(),
                    label: field.label.to_owned(),
                    help: field.help.to_owned(),
                    required: field.required,
                    secret: field.secret,
                    default: field.default.unwrap_or_default().to_owned(),
                    choices: field
                        .choices
                        .iter()
                        .map(|choice| ChoiceInfo {
                            value: choice.value.to_owned(),
                            label: choice.label.to_owned(),
                            prefill: choice
                                .prefill
                                .iter()
                                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str) -> &'static ProviderSpec {
        find_provider(id).expect("catalog should contain this provider")
    }

    fn remote(name: &str, backend: &str) -> RemoteInfo {
        RemoteInfo {
            name: format!("{name}:"),
            backend: backend.to_owned(),
            label: backend.to_owned(),
        }
    }

    #[test]
    fn catalog_entries_are_internally_consistent() {
        let mut seen_ids: Vec<&str> = Vec::new();
        for entry in CATALOG {
            assert!(!entry.id.is_empty(), "provider id must not be empty");
            assert!(
                !seen_ids.contains(&entry.id),
                "duplicate provider id: {}",
                entry.id
            );
            seen_ids.push(entry.id);
            assert!(
                !entry.backend.is_empty(),
                "{} must name an rclone backend",
                entry.id
            );

            match entry.auth {
                AuthKind::Browser => {
                    assert!(
                        entry
                            .config_answers
                            .iter()
                            .any(|(key, value)| *key == "config_is_local" && *value == "true"),
                        "{} must answer config_is_local so rclone opens a browser",
                        entry.id
                    );
                    assert!(
                        entry.fields.is_empty(),
                        "{} signs in through a browser and should ask nothing",
                        entry.id
                    );
                }
                AuthKind::Fields => {
                    assert!(
                        entry.fields.iter().any(|field| field.required),
                        "{} must ask for at least one required field",
                        entry.id
                    );
                    assert!(
                        entry.config_answers.is_empty(),
                        "{} completes without any config questions",
                        entry.id
                    );
                }
            }

            for field in entry.fields {
                assert!(!field.key.is_empty(), "{} has a field with no key", entry.id);
                if let Some(default) = field.default {
                    if !field.choices.is_empty() {
                        assert!(
                            field.choices.iter().any(|choice| choice.value == default),
                            "{} field {} defaults to a value it does not offer",
                            entry.id,
                            field.key
                        );
                    }
                }
            }

            for key in entry.require_one_of {
                assert!(
                    entry.fields.iter().any(|field| &field.key == key),
                    "{} requires one of an unknown field {key}",
                    entry.id
                );
            }
        }
    }

    #[test]
    fn google_drive_keeps_its_original_remote_and_browsing_access() {
        let drive = provider("google_drive");
        assert_eq!(drive.backend, "drive");
        assert_eq!(remote_name_for(drive, &[]), "CloudFolder");
        assert!(drive
            .stored_options
            .iter()
            .any(|(key, value)| *key == "scope" && *value == "drive"));
    }

    #[test]
    fn other_providers_get_their_own_remote_names() {
        assert_eq!(remote_name_for(provider("dropbox"), &[]), "CloudFolder-Dropbox");
        assert_eq!(remote_name_for(provider("b2"), &[]), "CloudFolder-B2");
    }

    #[test]
    fn a_remote_of_the_same_backend_is_reused() {
        let existing = vec![remote("CloudFolder-Dropbox", "dropbox")];
        assert_eq!(
            remote_name_for(provider("dropbox"), &existing),
            "CloudFolder-Dropbox"
        );
    }

    #[test]
    fn a_remote_of_a_different_backend_is_stepped_over() {
        let existing = vec![
            remote("CloudFolder-Dropbox", "sftp"),
            remote("CloudFolder-Dropbox-2", "s3"),
        ];
        assert_eq!(
            remote_name_for(provider("dropbox"), &existing),
            "CloudFolder-Dropbox-3"
        );
    }

    #[test]
    fn missing_required_fields_are_rejected_before_rclone_runs() {
        let mut submitted = HashMap::new();
        submitted.insert("account".to_owned(), "  ".to_owned());
        submitted.insert("key".to_owned(), "secret-key".to_owned());
        let error = resolve_values(provider("b2"), &submitted)
            .expect_err("a blank key id should not be accepted");
        assert!(error.to_string().contains("Key ID"), "{error}");
    }

    #[test]
    fn defaults_fill_in_for_fields_left_alone() {
        let mut submitted = HashMap::new();
        submitted.insert("host".to_owned(), "backup.example.com".to_owned());
        submitted.insert("user".to_owned(), "ryan".to_owned());
        submitted.insert("pass".to_owned(), "hunter2".to_owned());
        let values = resolve_values(provider("sftp"), &submitted).expect("valid sftp details");
        assert!(values.contains(&("port".to_owned(), "22".to_owned())));
    }

    #[test]
    fn sftp_needs_either_a_password_or_a_key_file() {
        let mut submitted = HashMap::new();
        submitted.insert("host".to_owned(), "backup.example.com".to_owned());
        submitted.insert("user".to_owned(), "ryan".to_owned());
        let error = resolve_values(provider("sftp"), &submitted)
            .expect_err("neither a password nor a key file should be refused");
        assert!(error.to_string().contains("Password"), "{error}");

        submitted.insert("key_file".to_owned(), "/home/ryan/.ssh/id_ed25519".to_owned());
        assert!(resolve_values(provider("sftp"), &submitted).is_ok());
    }

    #[test]
    fn choices_outside_the_offered_list_are_rejected() {
        let mut submitted = HashMap::new();
        submitted.insert("provider".to_owned(), "NotARealCloud".to_owned());
        submitted.insert("access_key_id".to_owned(), "AKIA".to_owned());
        submitted.insert("secret_access_key".to_owned(), "shhh".to_owned());
        assert!(resolve_values(provider("s3"), &submitted).is_err());
    }

    #[test]
    fn secrets_never_survive_into_error_text() {
        let scrubbed = scrub_secrets(
            "auth failed for key sw0rdf1sh at host example.com",
            &["sw0rdf1sh".to_owned()],
        );
        assert!(!scrubbed.contains("sw0rdf1sh"));
        assert!(scrubbed.contains("example.com"));
    }

    #[test]
    fn very_short_secrets_are_left_alone_to_avoid_mangling_text() {
        let scrubbed = scrub_secrets("a connection error", &["a".to_owned()]);
        assert_eq!(scrubbed, "a connection error");
    }

    #[test]
    fn unknown_backends_still_get_a_usable_label() {
        assert_eq!(label_for("drive", "CloudFolder:"), "Google Drive");
        assert_eq!(label_for("seafile", "myseafile:"), "seafile");
        assert_eq!(label_for("", "homemade:"), "homemade");
    }
}
