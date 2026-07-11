use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use tempfile::TempDir;

fn yoetz() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("yoetz").unwrap()
}

struct CouncilFixture {
    _dir: TempDir,
    config_path: PathBuf,
    state_dir: PathBuf,
}

impl CouncilFixture {
    fn new() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let request = read_http_request(&mut stream);
                let (status, body) = if request.contains("fail-model") {
                    (
                        "400 Bad Request",
                        serde_json::json!({
                            "error": {
                                "message": "forced model failure",
                                "type": "invalid_request_error"
                            }
                        }),
                    )
                } else {
                    (
                        "200 OK",
                        serde_json::json!({
                        "id": "successful-response",
                        "object": "chat.completion",
                        "created": 0,
                        "model": "success-model",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "successful answer"},
                            "finish_reason": "stop"
                        }],
                        "usage": {
                            "prompt_tokens": 7,
                            "completion_tokens": 3,
                            "total_tokens": 10
                        }
                        }),
                    )
                };
                let body = body.to_string();
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\nx-litellm-response-cost: 0.25\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let state_dir = dir.path().join("state");
        fs::write(
            &config_path,
            format!(
                r#"
[providers.mock]
base_url = "http://{address}/v1"
api_key_env = "MOCK_API_KEY"
kind = "openai_compatible"

[registry]
auto_sync_secs = 0
"#
            ),
        )
        .unwrap();

        Self {
            _dir: dir,
            config_path,
            state_dir,
        }
    }

    fn command(&self) -> Command {
        self.command_with_models("success-model,fail-model")
    }

    fn command_with_models(&self, models: &str) -> Command {
        let mut command = yoetz();
        command
            .env("YOETZ_CONFIG_PATH", &self.config_path)
            .env("YOETZ_DIR", &self.state_dir)
            .env("MOCK_API_KEY", "test-key")
            .env_remove("OPENAI_API_KEY")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("GEMINI_API_KEY")
            .env_remove("OPENROUTER_API_KEY")
            .env_remove("XAI_API_KEY")
            .args([
                "--format",
                "json",
                "--allow-unknown",
                "council",
                "--prompt",
                "compare",
                "--provider",
                "mock",
                "--models",
                models,
            ]);
        command
    }
}

fn read_http_request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream.read(&mut chunk).unwrap();
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let header_end = header_end + 4;
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        if bytes.len() >= header_end + content_length {
            break;
        }
    }
    String::from_utf8(bytes).unwrap()
}

fn parse_stdout(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not JSON: {error}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn assert_model_artifacts(payload: &Value) {
    let session_dir = Path::new(payload["artifacts"]["session_dir"].as_str().unwrap());
    let success: Value =
        serde_json::from_slice(&fs::read(session_dir.join("models/success-model.json")).unwrap())
            .unwrap();
    assert_eq!(success["status"], "succeeded");
    assert_eq!(success["model"], "success-model");
    assert_eq!(success["content"], "successful answer");
    assert_eq!(success["usage"]["total_tokens"], 10);
    assert_eq!(success["usage"]["cost_usd"], 0.25);
    assert!(success.get("pricing").is_some());
    assert!(success["error"].is_null());

    let failed: Value =
        serde_json::from_slice(&fs::read(session_dir.join("models/fail-model.json")).unwrap())
            .unwrap();
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["model"], "fail-model");
    assert!(!failed["error"].as_str().unwrap().is_empty());
    assert!(failed.get("usage").is_some());
    assert!(failed.get("pricing").is_some());
}

#[test]
fn partial_ok_is_default_and_writes_summary_and_model_artifacts() {
    let fixture = CouncilFixture::new();
    let output = fixture.command().output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    assert!(stdout.find("\"results\"").unwrap() < stdout.find("\"errors\"").unwrap());
    let payload = parse_stdout(&output);
    assert_eq!(payload["summary"]["succeeded"], 1);
    assert_eq!(payload["summary"]["failed"], 1);
    assert_eq!(payload["summary"]["total"], 2);
    assert_eq!(payload["summary"]["cost_usd"], 0.25);
    assert!(payload["summary"]["elapsed_ms"].as_u64().is_some());
    assert_eq!(payload["results"].as_array().unwrap().len(), 1);
    assert_eq!(payload["errors"].as_array().unwrap().len(), 1);
    assert_model_artifacts(&payload);
}

#[test]
fn partial_fail_exits_nonzero_after_emitting_complete_council() {
    let fixture = CouncilFixture::new();
    let output = fixture
        .command()
        .args(["--partial", "fail"])
        .output()
        .unwrap();
    assert!(!output.status.success());

    let payload = parse_stdout(&output);
    assert_eq!(payload["summary"]["succeeded"], 1);
    assert_eq!(payload["summary"]["failed"], 1);
    assert_eq!(payload["summary"]["total"], 2);
    assert_eq!(payload["results"].as_array().unwrap().len(), 1);
    assert_eq!(payload["errors"].as_array().unwrap().len(), 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("--partial fail"));
    assert_model_artifacts(&payload);
}

#[test]
fn all_models_failed_remains_a_hard_error_under_partial_ok() {
    let fixture = CouncilFixture::new();
    let output = fixture.command_with_models("fail-model").output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("all council models failed"));

    let sessions = fixture.state_dir.join("sessions");
    let session_dir = fs::read_dir(sessions)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let failed: Value =
        serde_json::from_slice(&fs::read(session_dir.join("models/fail-model.json")).unwrap())
            .unwrap();
    assert_eq!(failed["status"], "failed");
    assert!(!failed["error"].as_str().unwrap().is_empty());
}
