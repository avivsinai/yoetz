#![cfg(unix)]

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::thread;
use tempfile::TempDir;

fn yoetz() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("yoetz").unwrap()
}

struct CursorFixture {
    _dir: TempDir,
    config_path: PathBuf,
    registry_path: PathBuf,
    state_dir: PathBuf,
    bin_dir: PathBuf,
    source_path: PathBuf,
    invocation_log: PathBuf,
}

impl CursorFixture {
    fn new() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                read_http_request(&mut stream);
                let body = serde_json::json!({
                    "id": "mock-response",
                    "object": "chat.completion",
                    "created": 0,
                    "model": "mock/model",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "api answer"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10}
                })
                .to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let registry_path = dir.path().join("registry.json");
        let state_dir = dir.path().join("state");
        let bin_dir = dir.path().join("bin");
        let source_path = dir.path().join("source.rs");
        let invocation_log = dir.path().join("cursor-invocations.log");
        fs::create_dir(&bin_dir).unwrap();
        fs::write(&source_path, "fn answer() -> u8 { 42 }\n").unwrap();
        fs::write(
            &config_path,
            format!(
                r#"
[providers.mock]
base_url = "http://{address}/v1"
api_key_env = "MOCK_API_KEY"
kind = "openai-compatible"

[registry]
auto_sync_secs = 0

[defaults]
max_output_tokens = 1024
"#
            ),
        )
        .unwrap();
        fs::write(
            &registry_path,
            r#"{"version":1,"updated_at":null,"models":[{"id":"mock/model","provider":"mock","pricing":{}}]}"#,
        )
        .unwrap();
        let cursor_agent = bin_dir.join("cursor-agent");
        fs::write(
            &cursor_agent,
            r#"#!/bin/sh
if [ -n "$CURSOR_INVOCATION_LOG" ]; then
  printf '%s\n' "$1" >> "$CURSOR_INVOCATION_LOG"
fi
if [ "$1" = "models" ]; then
  printf '%s\n' 'Available models' '' 'cursor-grok-4.6-xhigh - Cursor Grok 4.6 Extra High'
  exit 0
fi
printf '%s' '{"type":"result","subtype":"success","is_error":false,"result":"cursor answer","session_id":"cursor-session","usage":{"inputTokens":11,"outputTokens":4}}'
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&cursor_agent).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&cursor_agent, permissions).unwrap();

        Self {
            _dir: dir,
            config_path,
            registry_path,
            state_dir,
            bin_dir,
            source_path,
            invocation_log,
        }
    }

    fn command(&self) -> Command {
        let mut command = yoetz();
        command
            .env("YOETZ_CONFIG_PATH", &self.config_path)
            .env("YOETZ_REGISTRY_PATH", &self.registry_path)
            .env("YOETZ_DIR", &self.state_dir)
            .env("PATH", &self.bin_dir)
            .env("CURSOR_INVOCATION_LOG", &self.invocation_log)
            .env("MOCK_API_KEY", "test-key")
            .env_remove("OPENAI_API_KEY")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("GEMINI_API_KEY")
            .env_remove("OPENROUTER_API_KEY")
            .env_remove("XAI_API_KEY")
            .args(["--format", "json"]);
        command
    }

    fn fail_discovery(&self) {
        let cursor_agent = self.bin_dir.join("cursor-agent");
        fs::write(
            &cursor_agent,
            r#"#!/bin/sh
if [ -n "$CURSOR_INVOCATION_LOG" ]; then
  printf '%s\n' "$1" >> "$CURSOR_INVOCATION_LOG"
fi
printf '%s\n' 'discovery unavailable' >&2
exit 23
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&cursor_agent).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(cursor_agent, permissions).unwrap();
    }

    fn invocations(&self) -> Vec<String> {
        fs::read_to_string(&self.invocation_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn read_http_request(stream: &mut TcpStream) {
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
}

fn json_stdout(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not JSON: {error}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn lists_live_cursor_models() {
    let fixture = CursorFixture::new();
    let output = fixture
        .command()
        .args(["models", "list", "--provider", "cursor", "-s", "grok-4.6"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let payload = json_stdout(&output);
    assert_eq!(payload["models"].as_array().unwrap().len(), 1);
    assert_eq!(payload["models"][0]["id"], "cursor-grok-4.6-xhigh");
    assert_eq!(payload["models"][0]["provider"], "cursor");
}

#[test]
fn ask_maps_cursor_result_to_yoetz_contract() {
    let fixture = CursorFixture::new();
    let output = fixture
        .command()
        .args([
            "ask",
            "--no-session",
            "--provider",
            "cursor",
            "--model",
            "cursor-grok-4.6-xhigh",
            "--prompt",
            "review this",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload = json_stdout(&output);
    assert_eq!(payload["provider"], "cursor");
    assert_eq!(payload["model"], "cursor-grok-4.6-xhigh");
    assert_eq!(payload["content"], "cursor answer");
    assert_eq!(payload["usage"]["input_tokens"], 11);
    assert_eq!(payload["usage"]["output_tokens"], 4);
    assert_eq!(payload["usage"]["total_tokens"], 15);
    assert!(payload["usage"]["cost_usd"].is_null());
}

#[test]
fn ask_routes_cursor_prefix_without_api_registry() {
    let fixture = CursorFixture::new();
    let output = fixture
        .command()
        .args([
            "ask",
            "--no-session",
            "--model",
            "cursor/cursor-grok-4.6-xhigh",
            "--prompt",
            "review this",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload = json_stdout(&output);
    assert_eq!(payload["provider"], "cursor");
    assert_eq!(payload["content"], "cursor answer");
}

#[test]
fn council_routes_cursor_prefixed_model() {
    let fixture = CursorFixture::new();
    let output = fixture
        .command()
        .args([
            "council",
            "--models",
            "cursor/cursor-grok-4.6-xhigh",
            "--prompt",
            "review this",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload = json_stdout(&output);
    assert_eq!(payload["provider"], "cursor");
    assert_eq!(payload["summary"]["succeeded"], 1);
    assert_eq!(
        payload["results"][0]["model"],
        "cursor/cursor-grok-4.6-xhigh"
    );
    assert_eq!(payload["results"][0]["content"], "cursor answer");
}

#[test]
fn council_shares_cursor_discovery_across_members() {
    let fixture = CursorFixture::new();
    let output = fixture
        .command()
        .args([
            "council",
            "--models",
            "cursor/cursor-grok-4.6-xhigh,cursor/cursor-grok-4.6-xhigh,cursor/cursor-grok-4.6-xhigh",
            "--prompt",
            "review this",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let invocations = fixture.invocations();
    assert_eq!(
        invocations
            .iter()
            .filter(|arg| arg.as_str() == "models")
            .count(),
        1,
        "discovery invocations: {invocations:?}"
    );
    assert_eq!(
        invocations
            .iter()
            .filter(|arg| arg.as_str() != "models")
            .count(),
        3,
        "consult invocations: {invocations:?}"
    );
}

#[test]
fn council_caches_cursor_discovery_failure_for_all_members() {
    let fixture = CursorFixture::new();
    fixture.fail_discovery();
    let output = fixture
        .command()
        .args([
            "council",
            "--models",
            "cursor/cursor-grok-4.6-xhigh,cursor/cursor-grok-4.6-xhigh,cursor/cursor-grok-4.6-xhigh",
            "--prompt",
            "review this",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr
            .matches("Cursor CLI model discovery failed: discovery unavailable")
            .count(),
        3,
        "stderr: {stderr}"
    );
    assert_eq!(fixture.invocations(), vec!["models"]);
}

#[test]
fn council_mixes_cursor_and_api_backends() {
    let fixture = CursorFixture::new();
    let output = fixture
        .command()
        .args([
            "council",
            "--models",
            "cursor/cursor-grok-4.6-xhigh,mock/model",
            "--prompt",
            "review this",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload = json_stdout(&output);
    assert_eq!(payload["provider"], "mixed");
    assert_eq!(payload["summary"]["succeeded"], 2);
    assert_eq!(payload["results"][0]["content"], "cursor answer");
    assert_eq!(payload["results"][1]["content"], "api answer");
}

#[test]
fn review_file_uses_cursor_backend() {
    let fixture = CursorFixture::new();
    let output = fixture
        .command()
        .args([
            "review",
            "file",
            "--path",
            fixture.source_path.to_str().unwrap(),
            "--provider",
            "cursor",
            "--model",
            "cursor-grok-4.6-xhigh",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload = json_stdout(&output);
    assert_eq!(payload["provider"], "cursor");
    assert_eq!(payload["content"], "cursor answer");
    assert_eq!(payload["usage"]["total_tokens"], 15);
}

#[test]
fn unsupported_cursor_contract_fails_before_call() {
    let fixture = CursorFixture::new();
    let output = fixture
        .command()
        .args([
            "ask",
            "--no-session",
            "--provider",
            "cursor",
            "--model",
            "cursor-grok-4.6-xhigh",
            "--response-format",
            "json",
            "--prompt",
            "review this",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not support --response-format"));
}
