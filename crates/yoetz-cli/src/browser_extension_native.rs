use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process;
#[cfg(unix)]
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;

use crate::chatgpt_recipe::{
    ChatgptModelSelectionStatus, ChatgptRecipeSpec, ChatgptTransportPhase,
};
use yoetz_core::output::{write_jsonl, OutputFormat};
use yoetz_core::paths::home_dir;

pub const TRANSPORT_NAME: &str = "chrome-extension-native";
pub const PROTOCOL_VERSION: u32 = 1;
pub const EXTENSION_ID: &str = "njdakhppfigmloihiikbjmheejfndbfa";
pub const EXTENSION_KEY: &str = "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAujviQNA7EjHnfqpn3TM5IfgmHzOnvtu5pXg3Y1rS5koNJBT2PSG7FTGi9wD4oqNLVFehKm5h46vq1u1ACsMjAUrqMMUVvf7RUeqieUmfbtKRmx24N2blfz4b8KYpMlNUhf8IZ5TAFbvzy9NEO2KHAHCV6pP84E4lLBW2OQIDhqJd0FfS3Ecn91pbsH3tcsU6Gu+WiPEHLXZjPj85KcgQ+8qL0Xz83V5hEXIocMlCQ0RnMOfQIp5qUEIKgZ7qKqEjW2czNz48s5Fdgzbv95Lf09vat1NWiDHXZtDPWIa6TRjlKAAXIwsz5A/DJibzWiCgKiuOWmCgQPJgDidoyj/7RQIDAQAB";
pub const NATIVE_HOST_NAME: &str = "com.yoetz.chatgpt_native";
pub const SOCKET_FILENAME: &str = "chatgpt-native.sock";
pub const TOKEN_FILENAME: &str = "chatgpt-native.token";
pub const STATUS_FILENAME: &str = "chatgpt-native-status.json";
pub const WRAPPER_FILENAME: &str = "yoetz-chrome-native-host";
pub const INSTANCES_DIRNAME: &str = "instances";
pub const MAX_CHROME_NATIVE_EXTENSION_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_CHROME_NATIVE_HOST_MESSAGE_BYTES: usize = 1024 * 1024;
pub const MAX_FRAME_BYTES: usize = MAX_CHROME_NATIVE_EXTENSION_MESSAGE_BYTES;
pub const MAX_BUNDLE_BYTES: u64 = 5 * 1024 * 1024;
const CHUNK_BYTES: usize = 192 * 1024;
const CONTROL_READ_TIMEOUT: Duration = Duration::from_secs(10);
const RECIPE_READ_GRACE: Duration = Duration::from_secs(60);
#[cfg(unix)]
const MAX_UNIX_SOCKET_PATH_BYTES: usize = 100;

#[derive(Debug, Error)]
#[error("frame is too large: {len} bytes, max {max} bytes")]
struct FrameTooLargeError {
    len: usize,
    max: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct InstallHostResult {
    pub status: &'static str,
    pub native_host_name: &'static str,
    pub extension_id: &'static str,
    pub manifest_path: PathBuf,
    pub wrapper_path: PathBuf,
    pub socket_path: PathBuf,
    pub token_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExtensionStatus {
    pub status: &'static str,
    pub native_host_name: &'static str,
    pub extension_id: &'static str,
    pub hello_seen: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_profile_email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_profile_id: Option<String>,
    pub manifest_path: PathBuf,
    pub manifest_installed: bool,
    pub wrapper_path: PathBuf,
    pub wrapper_installed: bool,
    pub socket_path: PathBuf,
    pub socket_reachable: bool,
    pub token_path: PathBuf,
    pub token_present: bool,
    pub status_path: PathBuf,
    pub status_file_present: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub connected_instances: Vec<ExtensionInstanceStatus>,
    pub protocol_version: u32,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExtensionInstanceStatus {
    pub native_instance_id: String,
    pub socket_path: PathBuf,
    pub pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    pub protocol_version: u32,
    pub last_seen_ms: u128,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorReport {
    pub ok: bool,
    pub checks: Vec<DoctorCheck>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorCheck {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtensionRecipeResult {
    pub response: String,
    pub model_used: Option<String>,
    pub model_selection_status: ChatgptModelSelectionStatus,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProtocolEnvelope {
    pub protocol_version: u32,
    pub transport: String,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_token: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
}

impl ProtocolEnvelope {
    fn new(
        kind: impl Into<String>,
        job_id: Option<String>,
        run_id: Option<String>,
        payload: Value,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            transport: TRANSPORT_NAME.to_string(),
            request_id: new_id("req"),
            job_id,
            run_id,
            workspace_id: workspace_id().ok(),
            capability_token: None,
            kind: kind.into(),
            payload,
        }
    }

    fn with_token(mut self, token: String) -> Self {
        self.capability_token = Some(token);
        self
    }
}

pub fn install_host() -> Result<InstallHostResult> {
    #[cfg(unix)]
    {
        let paths = extension_paths()?;
        ensure_private_dir(&paths.state_dir)?;
        fs::create_dir_all(
            paths
                .manifest_path
                .parent()
                .context("native host manifest path must have a parent")?,
        )?;

        ensure_capability_token(&paths.token_path)?;
        write_wrapper(&paths.wrapper_path)?;
        let manifest = native_host_manifest(&paths.wrapper_path)?;
        fs::write(
            &paths.manifest_path,
            serde_json::to_string_pretty(&manifest)? + "\n",
        )
        .with_context(|| {
            format!(
                "write native host manifest {}",
                paths.manifest_path.display()
            )
        })?;

        Ok(InstallHostResult {
            status: "installed",
            native_host_name: NATIVE_HOST_NAME,
            extension_id: EXTENSION_ID,
            manifest_path: paths.manifest_path,
            wrapper_path: paths.wrapper_path,
            socket_path: paths.socket_path,
            token_path: paths.token_path,
        })
    }
    #[cfg(not(unix))]
    {
        bail!("chrome-extension-native install-host is currently supported on macOS/Linux only")
    }
}

pub fn status() -> Result<ExtensionStatus> {
    let paths = extension_paths()?;
    let token_present = paths.token_path.exists();
    let manifest_installed = paths.manifest_path.exists();
    let wrapper_installed = paths.wrapper_path.exists();
    let status_file_present = paths.status_path.exists();
    let connected_instances = connected_extension_instances(&paths);
    let socket_reachable = socket_reachable(&paths.socket_path) || !connected_instances.is_empty();
    let status_value = read_status_file(&paths.status_path);
    let latest_instance_with_hello = connected_instances
        .iter()
        .filter(|instance| instance_has_extension_hello(instance))
        .max_by_key(|instance| instance.last_seen_ms);
    let extension_value = status_value
        .as_ref()
        .and_then(|value| value.get("extension"))
        .and_then(Value::as_object);
    let legacy_hello_seen = connected_instances.is_empty()
        && socket_reachable
        && status_file_has_extension_hello(extension_value);
    let hello_seen = latest_instance_with_hello.is_some() || legacy_hello_seen;
    let extension_version = latest_instance_with_hello
        .and_then(|instance| instance.extension_version.clone())
        .or_else(|| {
            legacy_extension_status_string(legacy_hello_seen, extension_value, "extension_version")
        });
    let extension_instance_id = latest_instance_with_hello
        .and_then(|instance| instance.extension_instance_id.clone())
        .or_else(|| {
            legacy_extension_status_string(
                legacy_hello_seen,
                extension_value,
                "extension_instance_id",
            )
        });
    let extension_profile_email = latest_instance_with_hello
        .and_then(|instance| instance.profile_email.clone())
        .or_else(|| {
            legacy_extension_status_string(legacy_hello_seen, extension_value, "profile_email")
        });
    let extension_profile_id = latest_instance_with_hello
        .and_then(|instance| instance.profile_id.clone())
        .or_else(|| {
            legacy_extension_status_string(legacy_hello_seen, extension_value, "profile_id")
        });
    let version_mismatch = status_value
        .as_ref()
        .and_then(|value| value.get("version_mismatch"))
        .and_then(Value::as_str)
        .is_some();
    let manual_handoff = status_value
        .as_ref()
        .and_then(|value| value.get("last_manual_handoff"))
        .and_then(Value::as_object)
        .is_some();
    let status = if version_mismatch {
        "version_mismatch"
    } else if manual_handoff {
        "manual_handoff"
    } else if socket_reachable && hello_seen {
        "connected"
    } else if socket_reachable {
        "missing_extension"
    } else if manifest_installed && wrapper_installed && token_present {
        "disconnected"
    } else {
        "not_installed"
    };
    let detail = match status {
        "connected" => {
            if let Some(email) = &extension_profile_email {
                format!(
                    "native host socket is reachable and extension hello was observed for Chrome profile email {email}"
                )
            } else {
                "native host socket is reachable and extension hello was observed".to_string()
            }
        }
        "missing_extension" => {
            "native host socket is reachable, but no extension hello was observed".to_string()
        }
        "version_mismatch" => status_value
            .as_ref()
            .and_then(|value| value.get("version_mismatch"))
            .and_then(Value::as_str)
            .unwrap_or("extension/native protocol version mismatch")
            .to_string(),
        "manual_handoff" => status_value
            .as_ref()
            .and_then(|value| value.get("last_manual_handoff"))
            .and_then(|value| value.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("last ChatGPT job requires manual handoff")
            .to_string(),
        "disconnected" => "native host is installed but no extension connection is reachable".to_string(),
        _ => "run `yoetz browser extension install-host --chatgpt` and install/load the Chrome extension".to_string(),
    };
    Ok(ExtensionStatus {
        status,
        native_host_name: NATIVE_HOST_NAME,
        extension_id: EXTENSION_ID,
        hello_seen,
        extension_version,
        extension_instance_id,
        extension_profile_email,
        extension_profile_id,
        manifest_path: paths.manifest_path,
        manifest_installed,
        wrapper_path: paths.wrapper_path,
        wrapper_installed,
        socket_path: paths.socket_path,
        socket_reachable,
        token_path: paths.token_path,
        token_present,
        status_path: paths.status_path,
        status_file_present,
        connected_instances,
        protocol_version: PROTOCOL_VERSION,
        detail,
    })
}

pub fn doctor() -> Result<DoctorReport> {
    let paths = extension_paths()?;
    let status_value = read_status_file(&paths.status_path);
    let connected_instances = connected_extension_instances(&paths);
    let latest_instance_with_hello = connected_instances
        .iter()
        .filter(|instance| instance_has_extension_hello(instance))
        .max_by_key(|instance| instance.last_seen_ms);
    let extension_value = status_value
        .as_ref()
        .and_then(|value| value.get("extension"))
        .and_then(Value::as_object);
    let socket_is_reachable =
        socket_reachable(&paths.socket_path) || !connected_instances.is_empty();
    let legacy_hello_seen = connected_instances.is_empty()
        && socket_is_reachable
        && status_file_has_extension_hello(extension_value);
    let extension_status = latest_instance_with_hello
        .map(|instance| {
            let version = instance
                .extension_version
                .as_deref()
                .unwrap_or("unknown-extension-version");
            let extension_instance_id = instance
                .extension_instance_id
                .as_deref()
                .unwrap_or("unknown-extension-instance");
            match instance.profile_email.as_deref() {
                Some(email) if !email.is_empty() => {
                    format!("extension_version={version}, extension_instance_id={extension_instance_id}, chrome_profile_email={email}")
                }
                _ => format!("extension_version={version}, extension_instance_id={extension_instance_id}"),
            }
        })
        .or_else(|| {
            if !legacy_hello_seen {
                return None;
            }
            let value = extension_value?;
            let version = value.get("extension_version").and_then(Value::as_str)?;
            let extension_instance_id = value
                .get("extension_instance_id")
                .and_then(Value::as_str)
                .unwrap_or("unknown-extension-instance");
            let profile_email = value.get("profile_email").and_then(Value::as_str);
            Some(match profile_email {
                Some(email) if !email.is_empty() => {
                    format!("extension_version={version}, extension_instance_id={extension_instance_id}, chrome_profile_email={email}")
                }
                _ => format!("extension_version={version}, extension_instance_id={extension_instance_id}"),
            })
        })
        .unwrap_or_else(|| "no extension hello observed".to_string());
    let extension_protocol = latest_instance_with_hello
        .map(|instance| instance.protocol_version as u64)
        .or_else(|| {
            if !legacy_hello_seen {
                return None;
            }
            extension_value
                .and_then(|value| value.get("protocol_version"))
                .and_then(Value::as_u64)
        });
    let extension_instance_id = latest_instance_with_hello
        .and_then(|instance| instance.extension_instance_id.clone())
        .or_else(|| {
            legacy_extension_status_string(
                legacy_hello_seen,
                extension_value,
                "extension_instance_id",
            )
        });
    let version_detail = status_value
        .as_ref()
        .and_then(|value| value.get("version_mismatch"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("protocol_version={PROTOCOL_VERSION}"));
    let checks = vec![
        DoctorCheck {
            name: "manifest",
            ok: paths.manifest_path.exists(),
            detail: paths.manifest_path.display().to_string(),
        },
        DoctorCheck {
            name: "wrapper",
            ok: paths.wrapper_path.exists(),
            detail: paths.wrapper_path.display().to_string(),
        },
        wrapper_target_doctor_check(&paths.wrapper_path),
        token_doctor_check(&paths.token_path),
        DoctorCheck {
            name: "socket",
            ok: socket_is_reachable,
            detail: if connected_instances.is_empty() {
                paths.socket_path.display().to_string()
            } else {
                observed_extension_profiles(&connected_instances)
            },
        },
        DoctorCheck {
            name: "extension_hello",
            ok: extension_protocol.is_some(),
            detail: extension_status,
        },
        DoctorCheck {
            name: "version_compatible",
            ok: extension_protocol == Some(PROTOCOL_VERSION as u64),
            detail: version_detail,
        },
        DoctorCheck {
            name: "extension_instance_id",
            ok: extension_instance_id.is_some(),
            detail: extension_instance_id.unwrap_or_else(|| {
                "no extension instance id observed; reload the unpacked extension in chrome://extensions".to_string()
            }),
        },
        identity_permission_doctor_check(latest_instance_with_hello, legacy_hello_seen, extension_value),
        DoctorCheck {
            name: "stable_extension_id",
            ok: EXTENSION_ID == extension_id_from_public_key(EXTENSION_KEY)?,
            detail: EXTENSION_ID.to_string(),
        },
    ];
    let ok = checks.iter().all(|check| check.ok);
    Ok(DoctorReport { ok, checks })
}

pub fn reconnect(selector: ExtensionInstanceSelector<'_>) -> Result<Value> {
    let response = send_control_job("reconnect", json!({ "intent": "reconnect" }), selector)?;
    Ok(json!({
        "status": "ok",
        "transport": TRANSPORT_NAME,
        "response": response.payload,
    }))
}

pub fn reload_extension(selector: ExtensionInstanceSelector<'_>) -> Result<Value> {
    let response = send_control_job(
        "reconnect",
        json!({ "intent": "reload_extension" }),
        selector,
    )?;
    let reload_started =
        response.payload.get("status").and_then(Value::as_str) == Some("reloading");
    if !reload_started {
        bail!(
            "connected extension did not acknowledge reload; reload the unpacked Yoetz extension in chrome://extensions"
        );
    }
    Ok(json!({
        "status": "reloading",
        "transport": TRANSPORT_NAME,
        "response": response.payload,
    }))
}

pub fn canary(live: bool, selector: ExtensionInstanceSelector<'_>) -> Result<Value> {
    if !live {
        let response =
            send_control_job("reconnect", json!({ "intent": "dry_run_canary" }), selector)?;
        return Ok(json!({
            "status": "ok",
            "transport": TRANSPORT_NAME,
            "live": false,
            "response": response.payload,
        }));
    }

    let dir = tempfile::tempdir()?;
    let bundle_path = dir.path().join("yoetz-chatgpt-native-canary.md");
    fs::write(&bundle_path, "Reply with exactly OK.\n")?;
    let spec = ChatgptRecipeSpec {
        bundle_path: Some(bundle_path),
        model: "current".to_string(),
        prompt: "Reply with exactly OK.".to_string(),
        browser_context_id: None,
        profile_email: selector.profile_email.map(str::to_string),
        extension_instance_id: selector.extension_instance_id.map(str::to_string),
        extension_profile_id: selector.extension_profile_id.map(str::to_string),
        run_id: new_id("canary"),
        wait_timeout_ms: 180_000,
        wait_interval_ms: 1_000,
        upload_timeout_ms: 30_000,
        send_timeout_ms: 120_000,
        disable_extended: false,
    };
    let response = run_chatgpt_recipe(&spec, OutputFormat::Json)?;
    validate_canary_response(&response.response)?;
    Ok(json!({
        "status": "ok",
        "transport": TRANSPORT_NAME,
        "live": true,
        "expected_response": "OK",
        "response": response.response,
        "model_used": response.model_used,
        "model_selection_status": response.model_selection_status,
        "warnings": response.warnings,
    }))
}

pub fn inspect_run(run_id: &str, selector: ExtensionInstanceSelector<'_>) -> Result<Value> {
    let trimmed = run_id.trim();
    if trimmed.is_empty() {
        bail!("--run-id is required");
    }
    let response = send_control_job("inspect_run", json!({ "run_id": trimmed }), selector)?;
    Ok(json!({
        "status": "ok",
        "transport": TRANSPORT_NAME,
        "response": response.payload,
    }))
}

pub fn grant_identity_permission(selector: ExtensionInstanceSelector<'_>) -> Result<Value> {
    let response = send_control_job("request_identity_permission", json!({}), selector)?;
    Ok(json!({
        "status": "ok",
        "transport": TRANSPORT_NAME,
        "response": response.payload,
    }))
}

pub fn run_chatgpt_recipe(
    spec: &ChatgptRecipeSpec,
    format: OutputFormat,
) -> Result<ExtensionRecipeResult> {
    let bundle_path = spec
        .bundle_path
        .as_deref()
        .context("chrome-extension-native transport requires `--bundle`")?;
    let bundle = validate_bundle_path(bundle_path)?;
    let paths = extension_paths()?;
    let instance = select_extension_instance(
        &paths,
        ExtensionInstanceSelector {
            profile_email: spec.profile_email.as_deref(),
            extension_instance_id: spec.extension_instance_id.as_deref(),
            extension_profile_id: spec.extension_profile_id.as_deref(),
        },
    )?;
    let token = read_capability_token(&paths.token_path)?;
    let mut stream = connect_socket(&instance.socket_path).with_context(|| {
        format!(
            "chrome-extension-native bridge is not connected at {}. Run `yoetz browser extension doctor --chatgpt`, then open Chrome with the Yoetz extension enabled.",
            instance.socket_path.display()
        )
    })?;
    stream.set_read_timeout(Some(
        Duration::from_millis(spec.wait_timeout_ms).saturating_add(RECIPE_READ_GRACE),
    ))?;

    let job_id = new_id("job");
    let start = ProtocolEnvelope::new(
        "job_start",
        Some(job_id.clone()),
        Some(spec.run_id.clone()),
        json!({
            "recipe": "chatgpt",
            "bundle_path": bundle.path,
            "file_name": bundle.file_name,
            "bundle_size": bundle.size,
            "mime": bundle.mime,
            "prompt": spec.prompt,
            "model": spec.model,
            "browser_context_id": spec.browser_context_id,
            "profile_email": spec.profile_email,
            "extension_instance_id": spec.extension_instance_id,
            "extension_profile_id": spec.extension_profile_id,
            "disable_extended": spec.disable_extended,
            "wait_timeout_ms": spec.wait_timeout_ms,
            "wait_interval_ms": spec.wait_interval_ms,
            "upload_timeout_ms": spec.upload_timeout_ms,
            "send_timeout_ms": spec.send_timeout_ms,
        }),
    )
    .with_token(token);
    write_json_frame(&mut stream, &start)?;

    loop {
        let envelope = read_json_frame(&mut stream)?;
        validate_inbound_envelope(&envelope)?;
        match envelope.kind.as_str() {
            "job_progress" => emit_progress(format, &envelope)?,
            "job_complete" => return parse_recipe_result(envelope),
            "job_error" => return Err(job_error(envelope)),
            other => {
                if matches!(format, OutputFormat::Text | OutputFormat::Markdown) {
                    eprintln!("info: ignored chrome-extension-native event `{other}`");
                }
            }
        }
    }
}

pub fn serve_native_host_chatgpt() -> Result<()> {
    #[cfg(unix)]
    {
        native_host_unix::serve()
    }
    #[cfg(not(unix))]
    {
        bail!("chrome-extension-native native host is currently supported on macOS/Linux only")
    }
}

#[derive(Clone, Debug)]
struct ExtensionPaths {
    state_dir: PathBuf,
    instances_dir: PathBuf,
    manifest_path: PathBuf,
    wrapper_path: PathBuf,
    socket_path: PathBuf,
    token_path: PathBuf,
    status_path: PathBuf,
}

#[derive(Clone, Debug)]
struct BundleInfo {
    path: PathBuf,
    file_name: String,
    size: u64,
    mime: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ExtensionInstanceSelector<'a> {
    pub profile_email: Option<&'a str>,
    pub extension_instance_id: Option<&'a str>,
    pub extension_profile_id: Option<&'a str>,
}

fn extension_paths() -> Result<ExtensionPaths> {
    let state_dir = yoetz_state_dir()?.join("chrome-extension-native");
    let manifest_dir = chrome_native_messaging_manifest_dir()?;
    let socket_path = env::var("YOETZ_CHROME_EXTENSION_NATIVE_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_socket_path(&state_dir));
    Ok(ExtensionPaths {
        manifest_path: manifest_dir.join(format!("{NATIVE_HOST_NAME}.json")),
        wrapper_path: state_dir.join(WRAPPER_FILENAME),
        socket_path,
        token_path: state_dir.join(TOKEN_FILENAME),
        status_path: state_dir.join(STATUS_FILENAME),
        instances_dir: state_dir.join(INSTANCES_DIRNAME),
        state_dir,
    })
}

fn yoetz_state_dir() -> Result<PathBuf> {
    if let Ok(dir) = env::var("YOETZ_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Some(home) = home_dir() {
        return Ok(home.join(".yoetz"));
    }
    Ok(PathBuf::from(".yoetz"))
}

fn chrome_native_messaging_manifest_dir() -> Result<PathBuf> {
    if let Ok(dir) = env::var("YOETZ_CHROME_NATIVE_MESSAGING_DIR") {
        return Ok(PathBuf::from(dir));
    }
    #[cfg(unix)]
    let home = home_dir().context("could not resolve home directory")?;
    #[cfg(target_os = "macos")]
    {
        Ok(home
            .join("Library")
            .join("Application Support")
            .join("Google")
            .join("Chrome")
            .join("NativeMessagingHosts"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Ok(home
            .join(".config")
            .join("google-chrome")
            .join("NativeMessagingHosts"))
    }
    #[cfg(not(unix))]
    {
        bail!("Chrome native messaging manifest install is currently supported on macOS/Linux only")
    }
}

fn read_status_file(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn extension_status_string(
    extension_value: Option<&serde_json::Map<String, Value>>,
    key: &str,
) -> Option<String> {
    extension_value
        .and_then(|value| value.get(key))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn legacy_extension_status_string(
    legacy_hello_seen: bool,
    extension_value: Option<&serde_json::Map<String, Value>>,
    key: &str,
) -> Option<String> {
    if legacy_hello_seen {
        extension_status_string(extension_value, key)
    } else {
        None
    }
}

fn status_file_has_extension_hello(
    extension_value: Option<&serde_json::Map<String, Value>>,
) -> bool {
    extension_status_string(extension_value, "extension_instance_id").is_some()
        || extension_status_string(extension_value, "extension_version").is_some()
        || extension_status_string(extension_value, "extension_id").is_some()
}

fn identity_permission_doctor_check(
    latest_instance_with_hello: Option<&ExtensionInstanceStatus>,
    legacy_hello_seen: bool,
    extension_value: Option<&serde_json::Map<String, Value>>,
) -> DoctorCheck {
    let granted_from_instance = latest_instance_with_hello.map(|instance| {
        let has_email = instance
            .profile_email
            .as_deref()
            .is_some_and(|value| !value.is_empty());
        let has_id = instance
            .profile_id
            .as_deref()
            .is_some_and(|value| !value.is_empty());
        has_email || has_id
    });
    let granted_from_legacy = if legacy_hello_seen {
        extension_value.map(|value| {
            let has_email = value
                .get("profile_email")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty());
            let has_id = value
                .get("profile_id")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty());
            has_email || has_id
        })
    } else {
        None
    };
    let granted = granted_from_instance.or(granted_from_legacy);
    let detail = match granted {
        Some(true) => {
            "identity.email optional permission granted; profile_email/profile_id available as opt-in routing verifiers"
                .to_string()
        }
        Some(false) => {
            "identity.email optional permission not granted; routing relies on extension_instance_id only. Run `yoetz browser extension grant-identity --chatgpt` to opt in to profile_email verification."
                .to_string()
        }
        None => {
            "no extension hello observed yet; identity.email permission status unknown".to_string()
        }
    };
    // identity.email is an optional permission — its absence is expected and never fails the doctor.
    DoctorCheck {
        name: "identity_permission_granted",
        ok: true,
        detail,
    }
}

fn instance_has_extension_hello(instance: &ExtensionInstanceStatus) -> bool {
    instance
        .extension_instance_id
        .as_deref()
        .is_some_and(|value| !value.is_empty())
        || instance
            .extension_version
            .as_deref()
            .is_some_and(|value| !value.is_empty())
}

fn connected_extension_instances(paths: &ExtensionPaths) -> Vec<ExtensionInstanceStatus> {
    let Ok(entries) = fs::read_dir(&paths.instances_dir) else {
        return Vec::new();
    };
    let mut instances = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let text = fs::read_to_string(path).ok()?;
            serde_json::from_str::<ExtensionInstanceStatus>(&text).ok()
        })
        .filter(|instance| instance.protocol_version == PROTOCOL_VERSION)
        .filter(|instance| socket_reachable(&instance.socket_path))
        .collect::<Vec<_>>();
    instances.sort_by(|a, b| a.native_instance_id.cmp(&b.native_instance_id));
    instances
}

fn select_extension_instance(
    paths: &ExtensionPaths,
    selector: ExtensionInstanceSelector<'_>,
) -> Result<ExtensionInstanceStatus> {
    let instances = connected_extension_instances(paths);
    let requested_email = non_empty_selector(selector.profile_email);
    let requested_extension_instance_id = non_empty_selector(selector.extension_instance_id);
    let requested_extension_profile_id = non_empty_selector(selector.extension_profile_id);

    if requested_email.is_some()
        || requested_extension_instance_id.is_some()
        || requested_extension_profile_id.is_some()
    {
        let matches = instances
            .iter()
            .filter(|instance| {
                selector_matches_instance(
                    instance,
                    requested_email,
                    requested_extension_instance_id,
                    requested_extension_profile_id,
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        return match matches.len() {
            1 => Ok(matches[0].clone()),
            0 => {
                let observed = observed_extension_profiles(&instances);
                let selector = describe_instance_selector(
                    requested_email,
                    requested_extension_instance_id,
                    requested_extension_profile_id,
                );
                bail!(
                    "chrome-extension-native cannot route requested {selector}; no connected extension instance matched. Connected instances: {observed}"
                )
            }
            _ => bail!(
                "chrome-extension-native found multiple connected extension instances for {}; reload duplicate extension profiles and retry",
                describe_instance_selector(
                    requested_email,
                    requested_extension_instance_id,
                    requested_extension_profile_id,
                )
            ),
        };
    }

    match instances.len() {
        1 => Ok(instances[0].clone()),
        0 => connect_legacy_socket_instance(paths),
        _ => bail!(
            "chrome-extension-native found multiple connected extension profiles; pass --var profile_email=<chrome-profile-email> or --var extension_instance_id=<id> so Yoetz can route the job safely. Connected instances: {}",
            observed_extension_profiles(&instances)
        ),
    }
}

fn non_empty_selector(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn selector_matches_instance(
    instance: &ExtensionInstanceStatus,
    requested_email: Option<&str>,
    requested_extension_instance_id: Option<&str>,
    requested_extension_profile_id: Option<&str>,
) -> bool {
    requested_email.is_none_or(|email| {
        instance
            .profile_email
            .as_deref()
            .is_some_and(|actual| actual.eq_ignore_ascii_case(email))
    }) && requested_extension_instance_id.is_none_or(|id| {
        instance
            .extension_instance_id
            .as_deref()
            .is_some_and(|actual| actual == id)
    }) && requested_extension_profile_id.is_none_or(|id| {
        instance
            .profile_id
            .as_deref()
            .is_some_and(|actual| actual == id)
    })
}

fn describe_instance_selector(
    profile_email: Option<&str>,
    extension_instance_id: Option<&str>,
    extension_profile_id: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    if let Some(value) = profile_email {
        parts.push(format!("profile_email {value}"));
    }
    if let Some(value) = extension_instance_id {
        parts.push(format!("extension_instance_id {value}"));
    }
    if let Some(value) = extension_profile_id {
        parts.push(format!("extension_profile_id {value}"));
    }
    parts.join(" + ")
}

fn connect_legacy_socket_instance(paths: &ExtensionPaths) -> Result<ExtensionInstanceStatus> {
    if !socket_reachable(&paths.socket_path) {
        bail!(
            "chrome-extension-native bridge is not connected at {}. Run `yoetz browser extension doctor --chatgpt`, then open Chrome with the Yoetz extension enabled.",
            paths.socket_path.display()
        );
    }
    Ok(ExtensionInstanceStatus {
        native_instance_id: "legacy".to_string(),
        socket_path: paths.socket_path.clone(),
        pid: 0,
        extension_instance_id: None,
        extension_version: None,
        profile_email: None,
        profile_id: None,
        protocol_version: PROTOCOL_VERSION,
        last_seen_ms: 0,
    })
}

fn observed_extension_profiles(instances: &[ExtensionInstanceStatus]) -> String {
    if instances.is_empty() {
        return "none".to_string();
    }
    instances
        .iter()
        .map(|instance| {
            let email = instance.profile_email.as_deref().unwrap_or("<unknown>");
            let profile_id = instance.profile_id.as_deref().unwrap_or("<unknown>");
            let extension_instance_id = instance
                .extension_instance_id
                .as_deref()
                .unwrap_or("<no-extension-instance-id>");
            format!(
                "{}:{} (email={email}, profile_id={profile_id})",
                instance.native_instance_id, extension_instance_id
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn write_status_file(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(value)? + "\n")
        .with_context(|| format!("write native host status {}", path.display()))
}

fn merge_status_file(path: &Path, patch: Value) -> Result<()> {
    let mut value = read_status_file(path).unwrap_or_else(|| json!({}));
    let target = value
        .as_object_mut()
        .context("status file must be a JSON object")?;
    let patch = patch
        .as_object()
        .context("status patch must be a JSON object")?;
    for (key, value) in patch {
        target.insert(key.clone(), value.clone());
    }
    write_status_file(path, &value)
}

fn native_host_manifest(wrapper_path: &Path) -> Result<Value> {
    Ok(json!({
        "name": NATIVE_HOST_NAME,
        "description": "Yoetz ChatGPT native bridge",
        "path": wrapper_path.canonicalize().unwrap_or_else(|_| wrapper_path.to_path_buf()),
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{EXTENSION_ID}/")],
    }))
}

fn default_socket_path(state_dir: &Path) -> PathBuf {
    let state_socket = state_dir.join(SOCKET_FILENAME);
    #[cfg(unix)]
    {
        if unix_socket_path_fits(&state_socket) {
            state_socket
        } else {
            short_socket_path(state_dir)
        }
    }
    #[cfg(not(unix))]
    {
        state_socket
    }
}

#[cfg(unix)]
fn unix_socket_path_fits(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().len() < MAX_UNIX_SOCKET_PATH_BYTES
}

#[cfg(unix)]
fn short_socket_path(state_dir: &Path) -> PathBuf {
    let digest = socket_fallback_digest(state_dir);
    socket_fallback_dir(state_dir).join(format!("{}.sock", &digest[..16]))
}

#[cfg(unix)]
fn socket_fallback_dir(state_dir: &Path) -> PathBuf {
    let digest = socket_fallback_digest(state_dir);
    env::temp_dir().join(format!("yoetz-cen-{}", &digest[..8]))
}

#[cfg(unix)]
fn socket_fallback_digest(state_dir: &Path) -> String {
    let mut hash = Sha256::new();
    hash.update(state_dir.to_string_lossy().as_bytes());
    hex::encode(hash.finalize())
}

#[cfg(unix)]
fn write_wrapper(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let exe = env::current_exe().context("resolve current yoetz executable")?;
    let mut script = "#!/bin/sh\n".to_string();
    for key in ["YOETZ_DIR", "YOETZ_CHROME_EXTENSION_NATIVE_SOCKET"] {
        if let Ok(value) = env::var(key) {
            script.push_str(&format!("export {key}={}\n", shell_quote(&value)));
        }
    }
    script.push_str(&format!(
        "exec {} browser chrome-native-host --chatgpt\n",
        shell_quote(&exe.to_string_lossy())
    ));
    fs::write(path, script).with_context(|| format!("write wrapper {}", path.display()))?;
    set_private_file_mode(path, 0o700)?;
    Ok(())
}

#[cfg(unix)]
fn set_private_file_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn ensure_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect directory {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "private directory must not be a symlink: {}",
            path.display()
        );
    }
    if !metadata.is_dir() {
        bail!("private directory must be a directory: {}", path.display());
    }
    let current_uid = unsafe { libc::geteuid() };
    if metadata.uid() != current_uid {
        bail!(
            "private directory {} is owned by uid {}, current uid is {}",
            path.display(),
            metadata.uid(),
            current_uid
        );
    }
    set_private_file_mode(path, 0o700)?;
    let mode = fs::symlink_metadata(path)?.permissions().mode() & 0o777;
    if mode != 0o700 {
        bail!(
            "private directory {} mode is {:o}, expected 700",
            path.display(),
            mode
        );
    }
    Ok(())
}

fn ensure_capability_token(path: &Path) -> Result<String> {
    if path.exists() {
        return read_capability_token(path);
    }
    if let Some(parent) = path.parent() {
        #[cfg(unix)]
        ensure_private_dir(parent)?;
        #[cfg(not(unix))]
        fs::create_dir_all(parent)?;
    }
    let token = hex::encode(rand::random::<[u8; 32]>());
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("create capability token {}", path.display()))?;
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    #[cfg(unix)]
    set_private_file_mode(path, 0o600)?;
    Ok(token)
}

fn read_capability_token(path: &Path) -> Result<String> {
    validate_private_token_file(path)?;
    let token = fs::read_to_string(path)
        .with_context(|| format!("read capability token {}", path.display()))?
        .trim()
        .to_string();
    if token.len() < 32 {
        bail!("capability token at {} is invalid", path.display());
    }
    Ok(token)
}

fn wrapper_target_doctor_check(path: &Path) -> DoctorCheck {
    let current_exe = match env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            return DoctorCheck {
                name: "wrapper_target",
                ok: false,
                detail: format!("could not resolve current yoetz executable: {err}"),
            };
        }
    };
    let expected_line = format!(
        "exec {} browser chrome-native-host --chatgpt",
        shell_quote(&current_exe.to_string_lossy())
    );
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            return DoctorCheck {
                name: "wrapper_target",
                ok: false,
                detail: format!("{}: {err}", path.display()),
            };
        }
    };
    if text.lines().any(|line| line == expected_line) {
        return DoctorCheck {
            name: "wrapper_target",
            ok: true,
            detail: current_exe.display().to_string(),
        };
    }
    let actual = text
        .lines()
        .find(|line| line.starts_with("exec "))
        .unwrap_or("<missing exec line>");
    DoctorCheck {
        name: "wrapper_target",
        ok: false,
        detail: format!(
            "wrapper targets `{actual}`; rerun `yoetz browser extension install-host --chatgpt` with {}",
            current_exe.display()
        ),
    }
}

fn token_doctor_check(path: &Path) -> DoctorCheck {
    match validate_private_token_file(path) {
        Ok(()) => DoctorCheck {
            name: "capability_token",
            ok: true,
            detail: path.display().to_string(),
        },
        Err(err) => DoctorCheck {
            name: "capability_token",
            ok: false,
            detail: format!("{}: {err}", path.display()),
        },
    }
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn validate_private_token_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect capability token {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("capability token must not be a symlink");
    }
    if !metadata.is_file() {
        bail!("capability token must be a regular file");
    }
    let current_uid = unsafe { libc::geteuid() };
    if metadata.uid() != current_uid {
        bail!(
            "capability token is owned by uid {}, current uid is {}",
            metadata.uid(),
            current_uid
        );
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        bail!(
            "capability token permissions are {mode:03o}; run `chmod 600 {}`",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_token_file(path: &Path) -> Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        bail!("capability token must be a regular file")
    }
}

fn validate_bundle_path(path: &Path) -> Result<BundleInfo> {
    let metadata = fs::metadata(path).with_context(|| format!("read bundle {}", path.display()))?;
    if !metadata.is_file() {
        bail!("bundle path is not a file: {}", path.display());
    }
    if metadata.len() > MAX_BUNDLE_BYTES {
        bail!(
            "bundle is {} bytes, above chrome-extension-native limit of {} bytes",
            metadata.len(),
            MAX_BUNDLE_BYTES
        );
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("bundle path must end in a UTF-8 filename")?
        .to_string();
    let mime = if file_name.ends_with(".md") || file_name.ends_with(".markdown") {
        "text/markdown"
    } else {
        "text/plain"
    }
    .to_string();
    Ok(BundleInfo {
        path: path.to_path_buf(),
        file_name,
        size: metadata.len(),
        mime,
    })
}

fn connect_socket(path: &Path) -> io::Result<SocketStream> {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        Ok(SocketStream::Unix(UnixStream::connect(path)?))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "chrome-extension-native socket is only supported on macOS/Linux",
        ))
    }
}

fn socket_reachable(path: &Path) -> bool {
    connect_socket(path).is_ok()
}

fn send_control_job(
    kind: &str,
    payload: Value,
    selector: ExtensionInstanceSelector<'_>,
) -> Result<ProtocolEnvelope> {
    let paths = extension_paths()?;
    let instance = select_extension_instance(&paths, selector)?;
    let token = read_capability_token(&paths.token_path)?;
    let job_id = new_id(kind);
    let mut stream = connect_socket(&instance.socket_path).with_context(|| {
        format!(
            "chrome-extension-native bridge is not connected at {}",
            instance.socket_path.display()
        )
    })?;
    stream.set_read_timeout(Some(CONTROL_READ_TIMEOUT))?;
    let envelope = ProtocolEnvelope::new(kind, Some(job_id), None, payload).with_token(token);
    write_json_frame(&mut stream, &envelope)?;
    loop {
        let response = read_json_frame(&mut stream)
            .with_context(|| format!("timed out waiting for chrome-extension-native `{kind}`"))?;
        validate_inbound_envelope(&response)?;
        match response.kind.as_str() {
            "job_progress" | "heartbeat" => continue,
            "job_complete" => return Ok(response),
            "job_error" => return Err(job_error(response)),
            _ => return Ok(response),
        }
    }
}

pub fn chunk_payloads_for_file(
    path: &Path,
    file_name: &str,
    mime_type: &str,
) -> Result<Vec<Value>> {
    let bytes = fs::read(path).with_context(|| format!("read bundle {}", path.display()))?;
    let total_bytes = bytes.len();
    let total_chunks = total_bytes.div_ceil(CHUNK_BYTES).max(1);
    let mut chunks = Vec::with_capacity(total_chunks);
    for sequence in 0..total_chunks {
        let start = sequence * CHUNK_BYTES;
        let end = (start + CHUNK_BYTES).min(total_bytes);
        let chunk = &bytes[start..end];
        chunks.push(json!({
            "sequence": sequence,
            "total_chunks": total_chunks,
            "total_bytes": total_bytes,
            "filename": file_name,
            "mime_type": mime_type,
            "bytes_base64": base64::engine::general_purpose::STANDARD.encode(chunk),
        }));
    }
    Ok(chunks)
}

fn validate_inbound_envelope(envelope: &ProtocolEnvelope) -> Result<()> {
    if envelope.protocol_version != PROTOCOL_VERSION {
        bail!(
            "chrome-extension-native protocol version mismatch: got {}, expected {}",
            envelope.protocol_version,
            PROTOCOL_VERSION
        );
    }
    if envelope.transport != TRANSPORT_NAME {
        bail!("unexpected transport `{}`", envelope.transport);
    }
    match envelope.kind.as_str() {
        "hello"
        | "heartbeat"
        | "job_start"
        | "job_progress"
        | "job_file_chunk"
        | "job_file_chunk_ack"
        | "job_complete"
        | "job_error"
        | "job_cancel"
        | "pair_request"
        | "pair_complete"
        | "reconnect"
        | "inspect_run"
        | "request_identity_permission" => {}
        other => bail!("unsupported chrome-extension-native envelope type `{other}`"),
    }
    Ok(())
}

fn parse_recipe_result(envelope: ProtocolEnvelope) -> Result<ExtensionRecipeResult> {
    let response = envelope
        .payload
        .get("response")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let model_used = envelope
        .payload
        .get("model_used")
        .and_then(Value::as_str)
        .map(str::to_string);
    let model_selection_status = parse_model_selection_status(
        envelope
            .payload
            .get("model_selection_status")
            .and_then(Value::as_str),
    );
    let warnings = envelope
        .payload
        .get("warnings")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(ExtensionRecipeResult {
        response,
        model_used,
        model_selection_status,
        warnings,
    })
}

fn validate_canary_response(response: &str) -> Result<()> {
    let trimmed = response.trim();
    if trimmed == "OK" {
        return Ok(());
    }
    bail!(
        "chrome-extension-native live canary expected exact response `OK`, got `{}`",
        trimmed
    )
}

fn parse_model_selection_status(value: Option<&str>) -> ChatgptModelSelectionStatus {
    match value.unwrap_or("unavailable") {
        "selected" => ChatgptModelSelectionStatus::Selected,
        "kept_current" => ChatgptModelSelectionStatus::KeptCurrent,
        "mismatch" => ChatgptModelSelectionStatus::Mismatch,
        _ => ChatgptModelSelectionStatus::Unavailable,
    }
}

fn job_error(envelope: ProtocolEnvelope) -> anyhow::Error {
    let message = envelope
        .payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("chrome-extension-native job failed");
    let phase = envelope.payload.get("phase").and_then(Value::as_str);
    let side_effect_started = envelope
        .payload
        .get("side_effect_started")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let err = anyhow!("{message}");
    if !side_effect_started {
        return err;
    }
    match phase {
        Some("upload") => {
            crate::chatgpt_recipe::mark_terminal_fallback_phase(err, ChatgptTransportPhase::Upload)
        }
        Some("send") => {
            crate::chatgpt_recipe::mark_terminal_fallback_phase(err, ChatgptTransportPhase::Send)
        }
        Some("wait_response") => crate::chatgpt_recipe::mark_terminal_fallback_phase(
            err,
            ChatgptTransportPhase::WaitResponse,
        ),
        _ => {
            crate::chatgpt_recipe::mark_terminal_fallback_phase(err, ChatgptTransportPhase::Upload)
        }
    }
}

fn emit_progress(format: OutputFormat, envelope: &ProtocolEnvelope) -> Result<()> {
    match format {
        OutputFormat::Jsonl => write_jsonl("browser.recipe", envelope),
        OutputFormat::Text | OutputFormat::Markdown => {
            if let Some(message) = envelope.payload.get("message").and_then(Value::as_str) {
                eprintln!("chrome-extension-native: {message}");
            }
            Ok(())
        }
        OutputFormat::Json => Ok(()),
    }
}

pub fn write_json_frame<W: Write>(writer: &mut W, value: &ProtocolEnvelope) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    write_frame(writer, &bytes)
}

pub fn read_json_frame<R: Read>(reader: &mut R) -> Result<ProtocolEnvelope> {
    let bytes = read_frame(reader)?;
    let envelope = serde_json::from_slice(&bytes)?;
    Ok(envelope)
}

pub fn write_frame<W: Write>(writer: &mut W, bytes: &[u8]) -> Result<()> {
    write_frame_with_limit(writer, bytes, MAX_FRAME_BYTES)
}

fn write_frame_with_limit<W: Write>(writer: &mut W, bytes: &[u8], max: usize) -> Result<()> {
    if bytes.len() > max {
        return Err(FrameTooLargeError {
            len: bytes.len(),
            max,
        }
        .into());
    }
    let len = u32::try_from(bytes.len()).context("frame length exceeds u32")?;
    writer.write_all(&len.to_ne_bytes())?;
    writer.write_all(bytes)?;
    writer.flush()?;
    Ok(())
}

pub fn read_frame<R: Read>(reader: &mut R) -> Result<Vec<u8>> {
    let mut len_buf = [0_u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_ne_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        let mut frame_body = reader.take(len as u64);
        io::copy(&mut frame_body, &mut io::sink())?;
        return Err(FrameTooLargeError {
            len,
            max: MAX_FRAME_BYTES,
        }
        .into());
    }
    let mut bytes = vec![0_u8; len];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn new_id(prefix: &str) -> String {
    let random = rand::random::<[u8; 8]>();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{prefix}_{now:x}_{}", hex::encode(random))
}

fn workspace_id() -> Result<String> {
    let cwd = env::current_dir()?;
    let mut hash = Sha256::new();
    hash.update(cwd.to_string_lossy().as_bytes());
    Ok(format!("workspace_{}", &hex::encode(hash.finalize())[..16]))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn extension_id_from_public_key(public_key_b64: &str) -> Result<String> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(public_key_b64)?;
    let digest = Sha256::digest(bytes);
    let mut id = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        id.push((b'a' + (byte >> 4)) as char);
        id.push((b'a' + (byte & 0x0f)) as char);
    }
    Ok(id)
}

enum SocketStream {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    #[cfg(not(unix))]
    Unsupported,
}

impl SocketStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        #[cfg(not(unix))]
        let _ = timeout;
        match self {
            #[cfg(unix)]
            SocketStream::Unix(stream) => stream.set_read_timeout(timeout),
            #[cfg(not(unix))]
            SocketStream::Unsupported => Err(unsupported_socket_error()),
        }
    }
}

impl Read for SocketStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(not(unix))]
        let _ = buf;
        match self {
            #[cfg(unix)]
            SocketStream::Unix(stream) => stream.read(buf),
            #[cfg(not(unix))]
            SocketStream::Unsupported => Err(unsupported_socket_error()),
        }
    }
}

impl Write for SocketStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cfg(not(unix))]
        let _ = buf;
        match self {
            #[cfg(unix)]
            SocketStream::Unix(stream) => stream.write(buf),
            #[cfg(not(unix))]
            SocketStream::Unsupported => Err(unsupported_socket_error()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            SocketStream::Unix(stream) => stream.flush(),
            #[cfg(not(unix))]
            SocketStream::Unsupported => Err(unsupported_socket_error()),
        }
    }
}

#[cfg(not(unix))]
fn unsupported_socket_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "chrome-extension-native socket is only supported on macOS/Linux",
    )
}

#[cfg(unix)]
mod native_host_unix {
    use super::*;
    use std::collections::HashMap;
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::thread;

    type Clients = Arc<Mutex<HashMap<String, ClientJob>>>;

    struct ClientJob {
        stream: UnixStream,
        job_id: String,
        run_id: Option<String>,
        chunks: Vec<Value>,
        next_chunk: usize,
        side_effect_started: bool,
        fallback_phase: Option<&'static str>,
        cancel_on_disconnect: bool,
    }

    struct RouteDelivery {
        stream: UnixStream,
        job_id: String,
        run_id: Option<String>,
        fallback_phase: Option<&'static str>,
        side_effect_started: bool,
        client_error: Option<ProtocolEnvelope>,
        next_chunk: Option<ProtocolEnvelope>,
        cancel_on_write_error: Option<ProtocolEnvelope>,
    }

    struct SocketFileGuard {
        path: PathBuf,
    }

    struct InstanceFileGuard {
        path: PathBuf,
    }

    struct NativeHostRuntime {
        native_instance_id: String,
        socket_path: PathBuf,
        instance_path: PathBuf,
    }

    impl SocketFileGuard {
        fn new(path: PathBuf) -> Self {
            Self { path }
        }
    }

    impl Drop for SocketFileGuard {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    impl Drop for InstanceFileGuard {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    pub(super) fn serve() -> Result<()> {
        let paths = extension_paths()?;
        ensure_private_dir(&paths.state_dir)?;
        ensure_private_dir(&paths.instances_dir)?;
        let token = ensure_capability_token(&paths.token_path)?;
        let native_instance_id = new_id("native");
        let (listener, socket_path) = bind_native_host_listener(&paths, &native_instance_id)?;
        let instance_path = paths
            .instances_dir
            .join(format!("{native_instance_id}.json"));
        let runtime = NativeHostRuntime {
            native_instance_id,
            socket_path,
            instance_path,
        };
        let _socket_guard = SocketFileGuard::new(runtime.socket_path.clone());
        let _instance_guard = InstanceFileGuard {
            path: runtime.instance_path.clone(),
        };
        fs::set_permissions(&runtime.socket_path, fs::Permissions::from_mode(0o600))?;

        let status = json!({
            "bridge_state": "native_host_started",
            "protocol_version": PROTOCOL_VERSION,
            "transport": TRANSPORT_NAME,
            "native_host_name": NATIVE_HOST_NAME,
            "extension_id": EXTENSION_ID,
            "pid": process::id(),
            "native_instance_id": runtime.native_instance_id,
            "socket_path": runtime.socket_path,
            "connected_at_ms": now_millis(),
        });
        write_status_file(&paths.status_path, &status)?;
        write_instance_status(&runtime, json!({}))?;

        let stdout = Arc::new(Mutex::new(io::stdout()));
        let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
        let accept_stdout = Arc::clone(&stdout);
        let accept_clients = Arc::clone(&clients);
        thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let token = token.clone();
                        let stdout = Arc::clone(&accept_stdout);
                        let clients = Arc::clone(&accept_clients);
                        thread::spawn(move || {
                            if let Err(err) = handle_client(stream, &token, stdout, clients) {
                                eprintln!("yoetz chrome native client error: {err:#}");
                            }
                        });
                    }
                    Err(err) => {
                        eprintln!("yoetz chrome native accept error: {err}");
                        break;
                    }
                }
            }
        });

        let mut stdin = io::stdin();
        loop {
            match read_json_frame(&mut stdin) {
                Ok(envelope) => {
                    if let Err(err) = validate_inbound_envelope(&envelope) {
                        record_protocol_mismatch(&paths, &envelope, &err)?;
                        continue;
                    }
                    route_extension_message(envelope, &clients, &stdout, &paths, &runtime)?;
                }
                Err(err) if is_disconnect_error(&err) => {
                    notify_clients_transport_lost(&clients);
                    let _ = fs::remove_file(&runtime.instance_path);
                    merge_status_file(
                        &paths.status_path,
                        json!({
                            "bridge_state": "native_host_stopped",
                            "disconnected_at_ms": now_millis(),
                        }),
                    )?;
                    return Ok(());
                }
                Err(err) if is_recoverable_input_error(&err) => {
                    eprintln!(
                        "yoetz chrome native ignored malformed Chrome native messaging frame: {err:#}"
                    );
                    merge_status_file(
                        &paths.status_path,
                        json!({
                            "last_native_host_input_error": {
                                "message": err.to_string(),
                                "seen_at_ms": now_millis(),
                                "recoverable": true,
                            },
                        }),
                    )?;
                    continue;
                }
                Err(err) => {
                    notify_clients_transport_lost(&clients);
                    let _ = fs::remove_file(&runtime.instance_path);
                    merge_status_file(
                        &paths.status_path,
                        json!({
                            "bridge_state": "native_host_input_error",
                            "last_native_host_input_error": {
                                "message": err.to_string(),
                                "seen_at_ms": now_millis(),
                            },
                        }),
                    )?;
                    return Err(err).context("read Chrome native messaging frame");
                }
            }
        }
    }

    pub(super) fn bind_native_host_listener(
        paths: &ExtensionPaths,
        native_instance_id: &str,
    ) -> Result<(UnixListener, PathBuf)> {
        let explicit_socket = env::var("YOETZ_CHROME_EXTENSION_NATIVE_SOCKET").is_ok();
        ensure_socket_parent_path(paths, &paths.socket_path)?;
        if explicit_socket || !active_socket_exists(&paths.socket_path) {
            remove_stale_socket(&paths.socket_path)?;
            let listener = UnixListener::bind(&paths.socket_path)
                .with_context(|| format!("bind {}", paths.socket_path.display()))?;
            return Ok((listener, paths.socket_path.clone()));
        }

        let socket_path = instance_socket_path(paths, native_instance_id);
        ensure_socket_parent_path(paths, &socket_path)?;
        remove_stale_socket(&socket_path)?;
        let listener = UnixListener::bind(&socket_path)
            .with_context(|| format!("bind {}", socket_path.display()))?;
        Ok((listener, socket_path))
    }

    fn active_socket_exists(path: &Path) -> bool {
        path.exists() && UnixStream::connect(path).is_ok()
    }

    fn instance_socket_path(paths: &ExtensionPaths, native_instance_id: &str) -> PathBuf {
        let state_socket = paths
            .instances_dir
            .join(format!("{native_instance_id}.sock"));
        if unix_socket_path_fits(&state_socket) {
            return state_socket;
        }
        socket_fallback_dir(&paths.state_dir).join(format!("{native_instance_id}.sock"))
    }

    pub(super) fn ensure_socket_parent_path(
        paths: &ExtensionPaths,
        socket_path: &Path,
    ) -> Result<()> {
        let Some(parent) = socket_path.parent() else {
            return Ok(());
        };
        if parent == paths.state_dir
            || parent == paths.instances_dir
            || parent == socket_fallback_dir(&paths.state_dir)
        {
            return ensure_private_dir(parent);
        }
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))
    }

    pub(super) fn remove_stale_socket(path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        if UnixStream::connect(path).is_ok() {
            bail!(
                "chrome-extension-native socket already has an active native host at {}",
                path.display()
            );
        }
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("read socket metadata {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!(
                "refusing to remove symlink at chrome-extension-native socket path {}",
                path.display()
            );
        }
        if !metadata.file_type().is_socket() {
            bail!(
                "refusing to remove non-socket file at chrome-extension-native socket path {}",
                path.display()
            );
        }
        fs::remove_file(path).with_context(|| format!("remove stale socket {}", path.display()))
    }

    fn handle_client(
        mut stream: UnixStream,
        token: &str,
        stdout: Arc<Mutex<io::Stdout>>,
        clients: Clients,
    ) -> Result<()> {
        let envelope = match read_json_frame(&mut stream) {
            Ok(envelope) => envelope,
            Err(err) if is_disconnect_error(&err) => return Ok(()),
            Err(err) => return Err(err),
        };
        validate_inbound_envelope(&envelope)?;
        if envelope.capability_token.as_deref() != Some(token) {
            bail!("capability token mismatch");
        }
        let job_id = envelope
            .job_id
            .clone()
            .context("local client message must include job_id")?;
        let (forwarded, chunks) = prepare_local_message(envelope.clone())?;
        let client = ClientJob {
            stream: stream.try_clone()?,
            job_id: job_id.clone(),
            run_id: envelope.run_id.clone(),
            chunks,
            next_chunk: 0,
            side_effect_started: false,
            fallback_phase: None,
            cancel_on_disconnect: envelope.kind == "job_start",
        };
        clients.lock().unwrap().insert(job_id.clone(), client);

        let forward_result = match envelope.kind.as_str() {
            "job_start"
            | "job_cancel"
            | "pair_request"
            | "reconnect"
            | "inspect_run"
            | "request_identity_permission" => forward_to_extension(&stdout, &forwarded),
            other => Err(anyhow!("unsupported local client message `{other}`")),
        };
        if let Err(err) = forward_result {
            if let Some(mut client) = clients.lock().unwrap().remove(&job_id) {
                let error = client_error_envelope(
                    &client,
                    "forward_to_extension_failed",
                    &format!("native host could not forward job to extension: {err}"),
                );
                let _ = write_json_frame(&mut client.stream, &error);
            }
            return Err(err);
        }

        stream.set_read_timeout(Some(Duration::from_secs(1)))?;
        loop {
            match read_json_frame(&mut stream) {
                Ok(control) => {
                    validate_inbound_envelope(&control)?;
                    if control.capability_token.as_deref() != Some(token) {
                        bail!("capability token mismatch");
                    }
                    if control.job_id.as_ref() != Some(&job_id) {
                        bail!("local client control message changed job_id");
                    }
                    let (forwarded, _) = prepare_local_message(control.clone())?;
                    match control.kind.as_str() {
                        "job_cancel"
                        | "reconnect"
                        | "inspect_run"
                        | "request_identity_permission" => {
                            if let Err(err) = forward_to_extension(&stdout, &forwarded) {
                                if let Some(mut client) = clients.lock().unwrap().remove(&job_id) {
                                    let error = client_error_envelope(
                                        &client,
                                        "forward_to_extension_failed",
                                        &format!(
                                            "native host could not forward control message to extension: {err}"
                                        ),
                                    );
                                    let _ = write_json_frame(&mut client.stream, &error);
                                }
                                return Err(err);
                            }
                        }
                        other => {
                            eprintln!("yoetz chrome native ignored local control event `{other}`")
                        }
                    }
                }
                Err(err) if is_timeout_error(&err) => {
                    if !clients.lock().unwrap().contains_key(&job_id) {
                        return Ok(());
                    }
                }
                Err(err) if is_disconnect_error(&err) => break,
                Err(err) => return Err(err).context("read local client control frame"),
            }
        }
        let still_active = clients.lock().unwrap().remove(&job_id).is_some();
        if still_active && envelope.kind == "job_start" {
            let cancel = ProtocolEnvelope::new(
                "job_cancel",
                Some(job_id),
                envelope.run_id.clone(),
                json!({
                    "reason": "local_client_disconnected"
                }),
            );
            forward_to_extension(&stdout, &cancel)?;
        }
        Ok(())
    }

    fn route_extension_message(
        envelope: ProtocolEnvelope,
        clients: &Clients,
        stdout: &Arc<Mutex<io::Stdout>>,
        paths: &ExtensionPaths,
        runtime: &NativeHostRuntime,
    ) -> Result<()> {
        let Some(job_id) = envelope.job_id.clone() else {
            record_unrouted_extension_message(paths, &envelope)?;
            record_instance_activity(runtime, &envelope)?;
            return Ok(());
        };
        let mut remove_client = matches!(
            envelope.kind.as_str(),
            "job_complete" | "job_error" | "job_cancel" | "pair_complete"
        );
        let delivery = {
            let mut clients = clients.lock().unwrap();
            if let Some(client) = clients.get_mut(&job_id) {
                update_client_effect_state(client, &envelope);
                let stream = match client.stream.try_clone() {
                    Ok(stream) => stream,
                    Err(err) => {
                        eprintln!(
                            "yoetz chrome native local client clone failed for {job_id}: {err:#}"
                        );
                        clients.remove(&job_id);
                        return Ok(());
                    }
                };
                let mut client_error = None;
                let mut next_chunk = None;
                match should_send_next_chunk(client, &envelope) {
                    Ok(true) => {
                        next_chunk = next_bundle_chunk_envelope(client)?;
                    }
                    Ok(false) => {}
                    Err(err) => {
                        client_error = Some(client_error_envelope(
                            client,
                            "chunk_ack_mismatch",
                            &format!("invalid extension chunk acknowledgement: {err}"),
                        ));
                        remove_client = true;
                    }
                }
                let cancel_on_write_error = if client.cancel_on_disconnect && !remove_client {
                    Some(local_client_disconnected_cancel(client))
                } else {
                    None
                };
                let delivery_job_id = client.job_id.clone();
                let delivery_run_id = client.run_id.clone();
                let delivery_fallback_phase = client.fallback_phase;
                let delivery_side_effect_started = client.side_effect_started;
                if remove_client {
                    clients.remove(&job_id);
                }
                Some(RouteDelivery {
                    stream,
                    job_id: delivery_job_id,
                    run_id: delivery_run_id,
                    fallback_phase: delivery_fallback_phase,
                    side_effect_started: delivery_side_effect_started,
                    client_error,
                    next_chunk,
                    cancel_on_write_error,
                })
            } else {
                None
            }
        };

        if let Some(mut delivery) = delivery {
            if let Err(err) = write_json_frame(&mut delivery.stream, &envelope) {
                eprintln!("yoetz chrome native local client write failed for {job_id}: {err:#}");
                if let Some(cancel) = delivery.cancel_on_write_error {
                    let _ = forward_to_extension(stdout, &cancel);
                }
                clients.lock().unwrap().remove(&job_id);
            } else if let Some(error) = delivery.client_error {
                let _ = write_json_frame(&mut delivery.stream, &error);
                remove_client = true;
            } else if let Some(chunk) = delivery.next_chunk {
                if let Err(err) = forward_to_extension(stdout, &chunk) {
                    eprintln!("yoetz chrome native chunk send failed for {job_id}: {err:#}");
                    let error = client_error_envelope_from_parts(
                        &delivery.job_id,
                        delivery.run_id.clone(),
                        "forward_to_extension_failed",
                        &format!("native host could not forward file chunk to extension: {err}"),
                        delivery.fallback_phase,
                        delivery.side_effect_started,
                    );
                    let _ = write_json_frame(&mut delivery.stream, &error);
                    remove_client = true;
                }
            }

            if remove_client {
                clients.lock().unwrap().remove(&job_id);
            }
        }

        if is_manual_handoff(&envelope) {
            record_manual_handoff(paths, &envelope)?;
        }
        record_instance_activity(runtime, &envelope)?;
        Ok(())
    }

    fn next_bundle_chunk_envelope(client: &mut ClientJob) -> Result<Option<ProtocolEnvelope>> {
        let Some(payload) = client.chunks.get(client.next_chunk).cloned() else {
            return Ok(None);
        };
        let chunk = ProtocolEnvelope::new(
            "job_file_chunk",
            Some(client.job_id.clone()),
            client.run_id.clone(),
            payload,
        );
        client.next_chunk += 1;
        Ok(Some(chunk))
    }

    fn prepare_local_message(
        mut envelope: ProtocolEnvelope,
    ) -> Result<(ProtocolEnvelope, Vec<Value>)> {
        let chunks = if envelope.kind == "job_start" {
            let bundle = validate_local_job_bundle(&envelope)?;
            if let Some(payload) = envelope.payload.as_object_mut() {
                payload.remove("bundle_path");
                payload.insert("file_name".to_string(), json!(bundle.file_name.clone()));
                payload.insert("bundle_size".to_string(), json!(bundle.size));
                payload.insert("mime".to_string(), json!(bundle.mime.clone()));
            }
            chunk_payloads_for_file(&bundle.path, &bundle.file_name, &bundle.mime)?
        } else {
            Vec::new()
        };
        Ok((without_token(envelope), chunks))
    }

    fn validate_local_job_bundle(envelope: &ProtocolEnvelope) -> Result<BundleInfo> {
        let bundle_path = envelope
            .payload
            .get("bundle_path")
            .and_then(Value::as_str)
            .context("job_start payload missing bundle_path")?;
        let bundle = validate_bundle_path(Path::new(bundle_path))?;
        if let Some(size) = envelope.payload.get("bundle_size").and_then(Value::as_u64) {
            if size != bundle.size {
                bail!(
                    "job_start bundle_size {} does not match current file size {}",
                    size,
                    bundle.size
                );
            }
        }
        Ok(bundle)
    }

    fn should_send_next_chunk(client: &ClientJob, envelope: &ProtocolEnvelope) -> Result<bool> {
        if envelope.kind == "job_progress" {
            if envelope.payload.get("phase").and_then(Value::as_str) == Some("ready_for_file") {
                return Ok(client.next_chunk == 0);
            }
            return Ok(false);
        }
        if envelope.kind == "job_file_chunk_ack" {
            if envelope
                .payload
                .get("complete")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Ok(false);
            }
            let sequence = envelope
                .payload
                .get("sequence")
                .and_then(Value::as_u64)
                .context("chunk ack missing sequence")?;
            if client.next_chunk == 0 {
                bail!("chunk ack arrived before any bundle chunk was sent");
            }
            let expected = (client.next_chunk - 1) as u64;
            if sequence < expected {
                return Ok(false);
            }
            if sequence > expected {
                bail!("chunk ack sequence {sequence} is ahead of expected {expected}");
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn local_client_disconnected_cancel(client: &ClientJob) -> ProtocolEnvelope {
        ProtocolEnvelope::new(
            "job_cancel",
            Some(client.job_id.clone()),
            client.run_id.clone(),
            json!({"reason": "local_client_disconnected"}),
        )
    }

    fn is_manual_handoff(envelope: &ProtocolEnvelope) -> bool {
        envelope
            .payload
            .get("phase")
            .and_then(Value::as_str)
            .is_some_and(|phase| phase == "manual_handoff")
            || envelope
                .payload
                .get("code")
                .and_then(Value::as_str)
                .is_some_and(|code| code == "manual_handoff")
    }

    fn update_client_effect_state(client: &mut ClientJob, envelope: &ProtocolEnvelope) {
        if envelope.kind != "job_progress" {
            return;
        }
        match envelope.payload.get("phase").and_then(Value::as_str) {
            Some(
                "tab_opened" | "model_selection" | "tab_grouped" | "ready_for_file"
                | "file_uploaded",
            ) => {
                client.side_effect_started = true;
                client.fallback_phase = Some("upload");
            }
            Some("prompt_sent") => {
                client.side_effect_started = true;
                client.fallback_phase = Some("send");
            }
            Some("manual_handoff") => {
                client.side_effect_started = true;
                client.fallback_phase.get_or_insert("upload");
            }
            _ => {}
        }
    }

    fn notify_clients_transport_lost(clients: &Clients) {
        let drained: Vec<ClientJob> = clients
            .lock()
            .unwrap()
            .drain()
            .map(|(_, client)| client)
            .collect();
        for mut client in drained {
            let error = client_error_envelope(
                &client,
                "native_host_disconnected",
                "Chrome native messaging connection closed before the job finished",
            );
            let _ = write_json_frame(&mut client.stream, &error);
        }
    }

    fn client_error_envelope(client: &ClientJob, code: &str, message: &str) -> ProtocolEnvelope {
        client_error_envelope_from_parts(
            &client.job_id,
            client.run_id.clone(),
            code,
            message,
            client.fallback_phase,
            client.side_effect_started,
        )
    }

    fn client_error_envelope_from_parts(
        job_id: &str,
        run_id: Option<String>,
        code: &str,
        message: &str,
        fallback_phase: Option<&'static str>,
        side_effect_started: bool,
    ) -> ProtocolEnvelope {
        ProtocolEnvelope::new(
            "job_error",
            Some(job_id.to_string()),
            run_id,
            json!({
                "code": code,
                "message": message,
                "phase": fallback_phase.unwrap_or("upload"),
                "side_effect_started": side_effect_started,
            }),
        )
    }

    fn record_manual_handoff(paths: &ExtensionPaths, envelope: &ProtocolEnvelope) -> Result<()> {
        merge_status_file(
            &paths.status_path,
            json!({
                "last_manual_handoff": {
                    "job_id": envelope.job_id,
                    "run_id": envelope.run_id,
                    "state": envelope.payload.get("state").cloned().unwrap_or(Value::Null),
                    "message": envelope.payload.get("message").cloned().unwrap_or(Value::Null),
                    "seen_at_ms": now_millis(),
                }
            }),
        )
    }

    fn record_unrouted_extension_message(
        paths: &ExtensionPaths,
        envelope: &ProtocolEnvelope,
    ) -> Result<()> {
        match envelope.kind.as_str() {
            "hello" => merge_status_file(
                &paths.status_path,
                json!({
                    "extension": {
                        "extension_id": envelope.payload.get("extension_id").cloned().unwrap_or(Value::Null),
                        "extension_version": envelope.payload.get("extension_version").cloned().unwrap_or(Value::Null),
                        "protocol_version": envelope.payload.get("protocol_version").cloned().unwrap_or(Value::Null),
                        "extension_instance_id": envelope.payload.get("extension_instance_id").cloned().unwrap_or(Value::Null),
                        "profile_email": envelope.payload.get("profile_email").cloned().unwrap_or(Value::Null),
                        "profile_id": envelope.payload.get("profile_id").cloned().unwrap_or(Value::Null),
                        "seen_at_ms": now_millis(),
                    },
                    "version_mismatch": Value::Null,
                    "last_manual_handoff": Value::Null,
                }),
            ),
            "heartbeat" => merge_status_file(
                &paths.status_path,
                json!({
                    "last_heartbeat_ms": now_millis(),
                }),
            ),
            _ => Ok(()),
        }
    }

    fn record_instance_activity(
        runtime: &NativeHostRuntime,
        envelope: &ProtocolEnvelope,
    ) -> Result<()> {
        match envelope.kind.as_str() {
            "hello" => write_instance_status(
                runtime,
                json!({
                    "extension_instance_id": envelope.payload.get("extension_instance_id").cloned().unwrap_or(Value::Null),
                    "extension_version": envelope.payload.get("extension_version").cloned().unwrap_or(Value::Null),
                    "profile_email": envelope.payload.get("profile_email").cloned().unwrap_or(Value::Null),
                    "profile_id": envelope.payload.get("profile_id").cloned().unwrap_or(Value::Null),
                    "protocol_version": envelope.payload.get("protocol_version").cloned().unwrap_or(json!(PROTOCOL_VERSION)),
                }),
            ),
            "heartbeat" | "job_progress" | "job_file_chunk_ack" | "job_complete" | "job_error" => {
                write_instance_status(runtime, json!({}))
            }
            _ => Ok(()),
        }
    }

    fn write_instance_status(runtime: &NativeHostRuntime, patch: Value) -> Result<()> {
        if let Some(parent) = runtime.instance_path.parent() {
            ensure_private_dir(parent)?;
        }
        let mut value = json!({
            "native_instance_id": runtime.native_instance_id,
            "socket_path": runtime.socket_path,
            "pid": process::id(),
            "extension_instance_id": Value::Null,
            "extension_version": Value::Null,
            "profile_email": Value::Null,
            "profile_id": Value::Null,
            "protocol_version": PROTOCOL_VERSION,
            "last_seen_ms": now_millis(),
        });
        if let Some(existing) = read_status_file(&runtime.instance_path) {
            value = existing;
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "native_instance_id".to_string(),
                    json!(runtime.native_instance_id),
                );
                object.insert("socket_path".to_string(), json!(runtime.socket_path));
                object.insert("pid".to_string(), json!(process::id()));
                object.insert("last_seen_ms".to_string(), json!(now_millis()));
            }
        }
        if let Some(object) = value.as_object_mut() {
            if let Some(patch) = patch.as_object() {
                for (key, value) in patch {
                    object.insert(key.clone(), value.clone());
                }
            }
        }
        fs::write(
            &runtime.instance_path,
            serde_json::to_string_pretty(&value)? + "\n",
        )
        .with_context(|| {
            format!(
                "write native host instance {}",
                runtime.instance_path.display()
            )
        })
    }

    fn record_protocol_mismatch(
        paths: &ExtensionPaths,
        envelope: &ProtocolEnvelope,
        err: &anyhow::Error,
    ) -> Result<()> {
        merge_status_file(
            &paths.status_path,
            json!({
                "version_mismatch": err.to_string(),
                "last_bad_protocol": {
                    "protocol_version": envelope.protocol_version,
                    "transport": envelope.transport,
                    "type": envelope.kind,
                    "seen_at_ms": now_millis(),
                }
            }),
        )
    }

    fn without_token(mut envelope: ProtocolEnvelope) -> ProtocolEnvelope {
        envelope.capability_token = None;
        envelope
    }

    fn forward_to_extension(
        stdout: &Arc<Mutex<io::Stdout>>,
        envelope: &ProtocolEnvelope,
    ) -> Result<()> {
        let bytes = serde_json::to_vec(envelope)?;
        let mut stdout = stdout.lock().unwrap();
        write_frame_with_limit(&mut *stdout, &bytes, MAX_CHROME_NATIVE_HOST_MESSAGE_BYTES)
    }

    fn is_timeout_error(err: &anyhow::Error) -> bool {
        matches!(
            err.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
        )
    }

    fn is_disconnect_error(err: &anyhow::Error) -> bool {
        matches!(
            err.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::ConnectionReset
            )
        )
    }

    fn is_recoverable_input_error(err: &anyhow::Error) -> bool {
        err.downcast_ref::<serde_json::Error>().is_some()
            || err.downcast_ref::<FrameTooLargeError>().is_some()
    }

    fn now_millis() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let old = env::var(key).ok();
            env::set_var(key, value);
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(old) = &self.old {
                env::set_var(self.key, old);
            } else {
                env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn frame_round_trips_json_envelope() {
        let envelope = ProtocolEnvelope::new(
            "job_progress",
            Some("job_1".to_string()),
            Some("run_1".to_string()),
            json!({"message": "uploading"}),
        );
        let mut buf = Vec::new();
        write_json_frame(&mut buf, &envelope).unwrap();
        assert!(buf.len() > 4);
        let decoded = read_json_frame(&mut &buf[..]).unwrap();
        assert_eq!(decoded.kind, "job_progress");
        assert_eq!(decoded.payload["message"], "uploading");
    }

    #[test]
    fn oversized_frame_is_rejected() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&((MAX_FRAME_BYTES as u32) + 1).to_ne_bytes());
        let err = read_frame(&mut &bytes[..]).unwrap_err();
        assert!(err.to_string().contains("too large"));
    }

    #[test]
    fn chrome_native_stdout_limit_is_enforced_separately() {
        let mut buf = Vec::new();
        let oversized = vec![b'x'; MAX_CHROME_NATIVE_HOST_MESSAGE_BYTES + 1];
        let err =
            write_frame_with_limit(&mut buf, &oversized, MAX_CHROME_NATIVE_HOST_MESSAGE_BYTES)
                .unwrap_err();
        assert!(err.to_string().contains("too large"));
        assert!(buf.is_empty());
    }

    #[test]
    fn malformed_json_frame_does_not_desync_next_frame() {
        let envelope = ProtocolEnvelope::new(
            "heartbeat",
            Some("job_1".to_string()),
            Some("run_1".to_string()),
            json!({"status": "alive"}),
        );
        let mut buf = Vec::new();
        write_frame(&mut buf, b"{not-json").unwrap();
        write_json_frame(&mut buf, &envelope).unwrap();

        let mut cursor = &buf[..];
        let err = read_json_frame(&mut cursor).unwrap_err();
        assert!(err.downcast_ref::<serde_json::Error>().is_some());
        let decoded = read_json_frame(&mut cursor).unwrap();
        assert_eq!(decoded.kind, "heartbeat");
        assert_eq!(decoded.payload["status"], "alive");
    }

    #[test]
    fn native_host_manifest_uses_stable_extension_origin() {
        let manifest = native_host_manifest(Path::new("/tmp/yoetz-wrapper")).unwrap();
        assert_eq!(manifest["name"], NATIVE_HOST_NAME);
        assert_eq!(manifest["type"], "stdio");
        assert_eq!(
            manifest["allowed_origins"][0],
            format!("chrome-extension://{EXTENSION_ID}/")
        );
    }

    #[test]
    fn pinned_key_derives_expected_extension_id() {
        assert_eq!(
            extension_id_from_public_key(EXTENSION_KEY).unwrap(),
            EXTENSION_ID
        );
    }

    #[test]
    #[cfg(unix)]
    fn short_state_dir_keeps_socket_under_state_dir() {
        let state_dir = PathBuf::from("/tmp/yoetz-short-state");
        assert_eq!(
            default_socket_path(&state_dir),
            state_dir.join(SOCKET_FILENAME)
        );
    }

    #[test]
    #[cfg(unix)]
    fn long_state_dir_uses_short_hashed_socket_path() {
        let state_dir = PathBuf::from("/tmp").join("a".repeat(180));
        let socket = default_socket_path(&state_dir);

        assert_ne!(socket, state_dir.join(SOCKET_FILENAME));
        let parent_name = socket
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        assert!(parent_name.starts_with("yoetz-cen-"));
        assert!(unix_socket_path_fits(&socket));
        assert_eq!(
            socket.extension().and_then(|ext| ext.to_str()),
            Some("sock")
        );
    }

    #[test]
    #[cfg(unix)]
    fn hashed_socket_parent_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let long_state = dir.path().join("a".repeat(180));
        let paths = ExtensionPaths {
            state_dir: long_state.clone(),
            instances_dir: long_state.join(INSTANCES_DIRNAME),
            manifest_path: long_state.join("manifest.json"),
            wrapper_path: long_state.join("wrapper"),
            socket_path: default_socket_path(&long_state),
            token_path: long_state.join(TOKEN_FILENAME),
            status_path: long_state.join(STATUS_FILENAME),
        };
        let socket = socket_fallback_dir(&paths.state_dir).join("native-instance.sock");

        native_host_unix::ensure_socket_parent_path(&paths, &socket).unwrap();

        let mode = fs::metadata(socket.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    #[cfg(unix)]
    fn bind_validates_hashed_socket_parent_before_stale_cleanup() {
        use std::os::unix::net::UnixListener;

        let dir = TempDir::new().unwrap();
        let long_state = dir.path().join("b".repeat(180));
        let paths = ExtensionPaths {
            state_dir: long_state.clone(),
            instances_dir: long_state.join(INSTANCES_DIRNAME),
            manifest_path: long_state.join("manifest.json"),
            wrapper_path: long_state.join("wrapper"),
            socket_path: default_socket_path(&long_state),
            token_path: long_state.join(TOKEN_FILENAME),
            status_path: long_state.join(STATUS_FILENAME),
        };
        let fallback_dir = socket_fallback_dir(&paths.state_dir);
        if let Ok(metadata) = fs::symlink_metadata(&fallback_dir) {
            if metadata.file_type().is_symlink() || metadata.is_file() {
                fs::remove_file(&fallback_dir).unwrap();
            } else {
                fs::remove_dir_all(&fallback_dir).unwrap();
            }
        }
        let target_dir = dir.path().join("socket-target");
        fs::create_dir_all(&target_dir).unwrap();
        let target_socket = target_dir.join(
            paths
                .socket_path
                .file_name()
                .expect("fallback socket filename"),
        );
        {
            let listener = UnixListener::bind(&target_socket).unwrap();
            drop(listener);
        }
        std::os::unix::fs::symlink(&target_dir, &fallback_dir).unwrap();

        let err = native_host_unix::bind_native_host_listener(&paths, "native-test").unwrap_err();

        assert!(err
            .to_string()
            .contains("private directory must not be a symlink"));
        assert!(target_socket.exists());
        fs::remove_file(&fallback_dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    #[serial]
    fn explicit_socket_env_is_honored() {
        let dir = TempDir::new().unwrap();
        let explicit = dir.path().join("explicit.sock");
        let _socket_guard = EnvGuard::set("YOETZ_CHROME_EXTENSION_NATIVE_SOCKET", &explicit);
        let _manifest_guard = EnvGuard::set(
            "YOETZ_CHROME_NATIVE_MESSAGING_DIR",
            &dir.path().join("native-hosts"),
        );
        let _state_guard = EnvGuard::set("YOETZ_DIR", &dir.path().join("state"));

        let paths = extension_paths().unwrap();
        assert_eq!(paths.socket_path, explicit);
    }

    #[test]
    #[cfg(unix)]
    fn stale_socket_cleanup_rejects_regular_files() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("not-a-socket.sock");
        fs::write(&path, "do not delete").unwrap();

        let err = native_host_unix::remove_stale_socket(&path).unwrap_err();

        assert!(err.to_string().contains("refusing to remove non-socket"));
        assert!(path.exists());
    }

    #[test]
    #[cfg(unix)]
    fn stale_socket_cleanup_rejects_symlinks() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("target.sock");
        let path = dir.path().join("socket-symlink.sock");
        fs::write(&target, "do not delete").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();

        let err = native_host_unix::remove_stale_socket(&path).unwrap_err();

        assert!(err.to_string().contains("refusing to remove symlink"));
        assert!(path.exists());
    }

    #[test]
    #[serial]
    fn install_host_writes_manifest_wrapper_and_token_under_isolated_home() {
        let dir = TempDir::new().unwrap();
        let manifest_dir = dir.path().join("native-hosts");
        let state_dir = dir.path().join("state");
        let _manifest_guard = EnvGuard::set("YOETZ_CHROME_NATIVE_MESSAGING_DIR", &manifest_dir);
        let _state_guard = EnvGuard::set("YOETZ_DIR", &state_dir);

        let result = install_host().unwrap();
        assert!(result.manifest_path.exists());
        assert!(result.wrapper_path.exists());
        assert!(result.token_path.exists());
        let manifest = fs::read_to_string(result.manifest_path).unwrap();
        assert!(manifest.contains(EXTENSION_ID));
        let wrapper = fs::read_to_string(result.wrapper_path).unwrap();
        assert!(wrapper.contains("YOETZ_DIR="));
        let wrapper_target = doctor()
            .unwrap()
            .checks
            .into_iter()
            .find(|check| check.name == "wrapper_target")
            .unwrap();
        assert!(wrapper_target.ok, "{}", wrapper_target.detail);
        let token = fs::read_to_string(&result.token_path).unwrap();
        assert!(token.trim().len() >= 32);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let extension_state_dir = result.token_path.parent().unwrap();
            let state_mode = fs::metadata(extension_state_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let token_mode = fs::metadata(result.token_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(state_mode, 0o700);
            assert_eq!(token_mode, 0o600);
        }
    }

    #[test]
    #[serial]
    fn status_reports_not_installed_without_manifest() {
        let dir = TempDir::new().unwrap();
        let _manifest_guard = EnvGuard::set(
            "YOETZ_CHROME_NATIVE_MESSAGING_DIR",
            &dir.path().join("native-hosts"),
        );
        let _state_guard = EnvGuard::set("YOETZ_DIR", &dir.path().join("state"));
        let payload = status().unwrap();
        assert_eq!(payload.status, "not_installed");
        assert!(!payload.manifest_installed);
        assert!(!payload.token_present);
    }

    #[test]
    #[serial]
    fn stale_status_file_does_not_count_as_live_hello() {
        let dir = TempDir::new().unwrap();
        let _manifest_guard = EnvGuard::set(
            "YOETZ_CHROME_NATIVE_MESSAGING_DIR",
            &dir.path().join("native-hosts"),
        );
        let _state_guard = EnvGuard::set("YOETZ_DIR", &dir.path().join("state"));
        let paths = extension_paths().unwrap();
        write_status_file(
            &paths.status_path,
            &json!({
                "extension": {
                    "extension_id": EXTENSION_ID,
                    "extension_version": "0.2.0",
                    "protocol_version": PROTOCOL_VERSION,
                    "extension_instance_id": "ext_123",
                    "profile_email": "work@example.com",
                    "profile_id": "gaia_123",
                    "seen_at_ms": 1234,
                }
            }),
        )
        .unwrap();

        let payload = status().unwrap();

        assert!(!payload.hello_seen);
        assert_eq!(payload.extension_version, None);
        assert_eq!(payload.extension_instance_id, None);

        let extension_hello = doctor()
            .unwrap()
            .checks
            .into_iter()
            .find(|check| check.name == "extension_hello")
            .unwrap();
        assert!(!extension_hello.ok);
        assert_eq!(extension_hello.detail, "no extension hello observed");
    }

    #[test]
    #[cfg(unix)]
    #[serial]
    fn status_reports_extension_profile_fields_from_live_hello() {
        use std::os::unix::net::UnixListener;

        let dir = TempDir::new().unwrap();
        let _manifest_guard = EnvGuard::set(
            "YOETZ_CHROME_NATIVE_MESSAGING_DIR",
            &dir.path().join("native-hosts"),
        );
        let _state_guard = EnvGuard::set("YOETZ_DIR", &dir.path().join("state"));
        let paths = extension_paths().unwrap();
        fs::create_dir_all(&paths.instances_dir).unwrap();
        let socket = dir.path().join("work.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        write_instance_fixture(
            &paths,
            ExtensionInstanceStatus {
                native_instance_id: "native_work".to_string(),
                socket_path: socket,
                pid: 111,
                extension_instance_id: Some("ext_123".to_string()),
                extension_version: Some("0.2.0".to_string()),
                profile_email: Some("work@example.com".to_string()),
                profile_id: Some("gaia_123".to_string()),
                protocol_version: PROTOCOL_VERSION,
                last_seen_ms: 1234,
            },
        );

        let payload = status().unwrap();

        assert!(payload.hello_seen);
        assert_eq!(payload.extension_version.as_deref(), Some("0.2.0"));
        assert_eq!(payload.extension_instance_id.as_deref(), Some("ext_123"));
        assert_eq!(
            payload.extension_profile_email.as_deref(),
            Some("work@example.com")
        );
        assert_eq!(payload.extension_profile_id.as_deref(), Some("gaia_123"));

        let extension_hello = doctor()
            .unwrap()
            .checks
            .into_iter()
            .find(|check| check.name == "extension_hello")
            .unwrap();
        assert!(extension_hello.ok);
        assert_eq!(
            extension_hello.detail,
            "extension_version=0.2.0, extension_instance_id=ext_123, chrome_profile_email=work@example.com"
        );
    }

    #[test]
    #[cfg(unix)]
    #[serial]
    fn select_extension_instance_routes_by_profile_and_fails_closed_when_ambiguous() {
        use std::os::unix::net::UnixListener;

        let dir = TempDir::new().unwrap();
        let manifest_dir = dir.path().join("native-hosts");
        let state_dir = dir.path().join("state");
        let _manifest_guard = EnvGuard::set("YOETZ_CHROME_NATIVE_MESSAGING_DIR", &manifest_dir);
        let _state_guard = EnvGuard::set("YOETZ_DIR", &state_dir);
        let paths = extension_paths().unwrap();
        fs::create_dir_all(&paths.instances_dir).unwrap();
        let work_socket = dir.path().join("work.sock");
        let personal_socket = dir.path().join("personal.sock");
        let _work_listener = UnixListener::bind(&work_socket).unwrap();
        let _personal_listener = UnixListener::bind(&personal_socket).unwrap();
        write_instance_fixture(
            &paths,
            ExtensionInstanceStatus {
                native_instance_id: "native_work".to_string(),
                socket_path: work_socket.clone(),
                pid: 111,
                extension_instance_id: Some("ext_work".to_string()),
                extension_version: Some("0.4.0".to_string()),
                profile_email: Some("work@example.com".to_string()),
                profile_id: Some("work_profile".to_string()),
                protocol_version: PROTOCOL_VERSION,
                last_seen_ms: 2,
            },
        );
        write_instance_fixture(
            &paths,
            ExtensionInstanceStatus {
                native_instance_id: "native_personal".to_string(),
                socket_path: personal_socket.clone(),
                pid: 222,
                extension_instance_id: Some("ext_personal".to_string()),
                extension_version: Some("0.4.0".to_string()),
                profile_email: Some("personal@example.com".to_string()),
                profile_id: Some("personal_profile".to_string()),
                protocol_version: PROTOCOL_VERSION,
                last_seen_ms: 1,
            },
        );

        let err =
            select_extension_instance(&paths, ExtensionInstanceSelector::default()).unwrap_err();
        assert!(err
            .to_string()
            .contains("multiple connected extension profiles"));

        let selected = select_extension_instance(
            &paths,
            ExtensionInstanceSelector {
                profile_email: Some("WORK@EXAMPLE.COM"),
                ..ExtensionInstanceSelector::default()
            },
        )
        .unwrap();
        assert_eq!(selected.native_instance_id, "native_work");
        assert_eq!(selected.socket_path, work_socket);
    }

    #[test]
    #[cfg(unix)]
    #[serial]
    fn select_extension_instance_routes_by_stable_instance_id_when_email_is_unknown() {
        use std::os::unix::net::UnixListener;

        let dir = TempDir::new().unwrap();
        let manifest_dir = dir.path().join("native-hosts");
        let state_dir = dir.path().join("state");
        let _manifest_guard = EnvGuard::set("YOETZ_CHROME_NATIVE_MESSAGING_DIR", &manifest_dir);
        let _state_guard = EnvGuard::set("YOETZ_DIR", &state_dir);
        let paths = extension_paths().unwrap();
        fs::create_dir_all(&paths.instances_dir).unwrap();
        let work_socket = dir.path().join("work.sock");
        let personal_socket = dir.path().join("personal.sock");
        let _work_listener = UnixListener::bind(&work_socket).unwrap();
        let _personal_listener = UnixListener::bind(&personal_socket).unwrap();
        write_instance_fixture(
            &paths,
            ExtensionInstanceStatus {
                native_instance_id: "native_work".to_string(),
                socket_path: work_socket.clone(),
                pid: 111,
                extension_instance_id: Some("ext_work".to_string()),
                extension_version: Some("0.4.0".to_string()),
                profile_email: None,
                profile_id: None,
                protocol_version: PROTOCOL_VERSION,
                last_seen_ms: 2,
            },
        );
        write_instance_fixture(
            &paths,
            ExtensionInstanceStatus {
                native_instance_id: "native_personal".to_string(),
                socket_path: personal_socket,
                pid: 222,
                extension_instance_id: Some("ext_personal".to_string()),
                extension_version: Some("0.4.0".to_string()),
                profile_email: None,
                profile_id: None,
                protocol_version: PROTOCOL_VERSION,
                last_seen_ms: 1,
            },
        );

        let selected = select_extension_instance(
            &paths,
            ExtensionInstanceSelector {
                extension_instance_id: Some("ext_work"),
                ..ExtensionInstanceSelector::default()
            },
        )
        .unwrap();

        assert_eq!(selected.native_instance_id, "native_work");
        assert_eq!(selected.socket_path, work_socket);
    }

    #[test]
    fn validate_bundle_rejects_oversized_files() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bundle.md");
        fs::write(&path, vec![b'x'; (MAX_BUNDLE_BYTES + 1) as usize]).unwrap();
        let err = validate_bundle_path(&path).unwrap_err();
        assert!(err
            .to_string()
            .contains("above chrome-extension-native limit"));
    }

    #[test]
    fn chunk_payload_uses_base64() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bundle.md");
        fs::write(&path, b"abc").unwrap();
        let chunks = chunk_payloads_for_file(&path, "bundle.md", "text/markdown").unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0]["sequence"], 0);
        assert_eq!(chunks[0]["total_chunks"], 1);
        assert_eq!(chunks[0]["total_bytes"], 3);
        assert_eq!(chunks[0]["filename"], "bundle.md");
        assert_eq!(chunks[0]["bytes_base64"], "YWJj");
    }

    #[test]
    fn canary_response_validation_requires_exact_ok() {
        validate_canary_response("OK\n").unwrap();
        let err = validate_canary_response("OK.").unwrap_err();
        assert!(err.to_string().contains("expected exact response `OK`"));
    }

    fn write_instance_fixture(paths: &ExtensionPaths, instance: ExtensionInstanceStatus) {
        let path = paths
            .instances_dir
            .join(format!("{}.json", instance.native_instance_id));
        fs::write(path, serde_json::to_string_pretty(&instance).unwrap()).unwrap();
    }

    #[test]
    fn job_error_phase_is_terminal_only_after_side_effects() {
        let pre_effect = job_error(ProtocolEnvelope::new(
            "job_error",
            Some("job_pre".to_string()),
            Some("run_pre".to_string()),
            json!({
                "message": "file input missing before upload",
                "phase": "upload",
                "side_effect_started": false,
            }),
        ));
        assert!(crate::chatgpt_recipe::terminal_fallback_phase(&pre_effect).is_none());

        let post_effect = job_error(ProtocolEnvelope::new(
            "job_error",
            Some("job_post".to_string()),
            Some("run_post".to_string()),
            json!({
                "message": "send failed after upload",
                "phase": "send",
                "side_effect_started": true,
            }),
        ));
        assert_eq!(
            crate::chatgpt_recipe::terminal_fallback_phase(&post_effect),
            Some(ChatgptTransportPhase::Send)
        );
    }
}
