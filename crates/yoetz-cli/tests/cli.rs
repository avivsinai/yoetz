use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn yoetz() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("yoetz").unwrap()
}

struct FrontierFixture {
    _dir: TempDir,
    config_path: PathBuf,
    registry_path: PathBuf,
}

fn frontier_fixture() -> FrontierFixture {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("yoetz.toml");
    let registry_path = dir.path().join("registry.json");

    fs::write(
        &config_path,
        r#"
[registry]
auto_sync_secs = 0
"#,
    )
    .unwrap();

    fs::write(&registry_path, frontier_registry_json()).unwrap();

    FrontierFixture {
        _dir: dir,
        config_path,
        registry_path,
    }
}

fn frontier_registry_json() -> String {
    serde_json::json!({
        "version": 1,
        "updated_at": "2026-03-21T00:00:00Z",
        "models": [
            {
                "id": "openai/gpt-5.4-pro",
                "context_length": 256000,
                "max_output_tokens": 16384,
                "pricing": {"completion_per_1k": 0.18},
                "provider": "openrouter"
            },
            {
                "id": "openai/gpt-5.4-mini",
                "context_length": 128000,
                "max_output_tokens": 8192,
                "pricing": {"completion_per_1k": 0.02},
                "provider": "openrouter"
            },
            {
                "id": "anthropic/claude-opus-4-6",
                "context_length": 1000000,
                "max_output_tokens": 32000,
                "pricing": {"completion_per_1k": 0.025},
                "provider": "openrouter"
            },
            {
                "id": "anthropic/claude-sonnet-4-6",
                "context_length": 200000,
                "max_output_tokens": 16384,
                "pricing": {"completion_per_1k": 0.012},
                "provider": "openrouter"
            },
            {
                "id": "google/gemini-3.1-pro-preview",
                "context_length": 1000000,
                "max_output_tokens": 8192,
                "pricing": {"completion_per_1k": 0.012},
                "provider": "openrouter"
            },
            {
                "id": "x-ai/grok-4.20-beta",
                "context_length": 200000,
                "max_output_tokens": 16384,
                "pricing": {"completion_per_1k": 0.006},
                "provider": "openrouter"
            },
            {
                "id": "meta-llama/llama-3.1-405b",
                "context_length": 32000,
                "max_output_tokens": 8192,
                "pricing": {"completion_per_1k": 0.004},
                "provider": "openrouter"
            },
            {
                "id": "deepseek/deepseek-v3.2-speciale",
                "context_length": 163840,
                "max_output_tokens": 8192,
                "pricing": {"completion_per_1k": 0.001},
                "provider": "openrouter"
            },
            {
                "id": "mistralai/mistral-large-2411",
                "context_length": 131072,
                "max_output_tokens": 8192,
                "pricing": {"completion_per_1k": 0.006},
                "provider": "openrouter"
            },
            {
                "id": "qwen/qwen-max",
                "context_length": 32768,
                "max_output_tokens": 8192,
                "pricing": {"completion_per_1k": 0.004},
                "provider": "openrouter"
            },
            {
                "id": "moonshotai/kimi-k2",
                "context_length": 128000,
                "max_output_tokens": 8192,
                "pricing": {"completion_per_1k": 0.003},
                "provider": "openrouter"
            },
            {
                "id": "reseller/one-off-model",
                "context_length": 64000,
                "max_output_tokens": 4096,
                "pricing": {"completion_per_1k": 0.123},
                "provider": "reseller"
            }
        ]
    })
    .to_string()
}

fn yoetz_with_frontier_fixture(fixture: &FrontierFixture) -> Command {
    let mut cmd = yoetz();
    cmd.env("YOETZ_CONFIG_PATH", &fixture.config_path)
        .env("YOETZ_REGISTRY_PATH", &fixture.registry_path)
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("XAI_API_KEY");
    cmd
}

fn command_path(dir: &std::path::Path, name: &str) -> PathBuf {
    if cfg!(windows) {
        dir.join(format!("{name}.cmd"))
    } else {
        dir.join(name)
    }
}

fn write_executable_script(path: &std::path::Path, unix_contents: &str, windows_contents: &str) {
    let contents = if cfg!(windows) {
        windows_contents
    } else {
        unix_contents
    };
    fs::write(path, contents).unwrap();
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
}

fn fake_agent_browser_failure_bin(detail: &str) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let bin = command_path(dir.path(), "fake-agent-browser-fail");
    let unix = format!("#!/bin/sh\necho \"{detail}\" 1>&2\nexit 1\n");
    let windows = format!("@echo off\r\necho {detail} 1>&2\r\nexit /b 1\r\n");
    write_executable_script(&bin, &unix, &windows);
    (dir, bin)
}

#[test]
fn version_flag() {
    yoetz()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_flag() {
    yoetz()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("agent-friendly LLM council"));
}

#[test]
fn no_subcommand_shows_help() {
    yoetz()
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage"));
}

#[test]
fn ask_without_prompt_fails() {
    yoetz()
        .arg("ask")
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .assert()
        .failure();
}

#[test]
fn bundle_help() {
    yoetz()
        .args(["bundle", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--files"))
        .stdout(predicate::str::contains("--include-hidden"));
}

#[test]
fn council_help() {
    yoetz()
        .args(["council", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--models"));
}

#[test]
fn review_help() {
    yoetz().args(["review", "--help"]).assert().success();
}

#[test]
fn generate_help() {
    yoetz().args(["generate", "--help"]).assert().success();
}

#[test]
fn pricing_help() {
    yoetz().args(["pricing", "--help"]).assert().success();
}

#[test]
fn browser_recipe_help_mentions_var() {
    yoetz()
        .args(["browser", "recipe", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--var"));
}

#[test]
fn browser_attach_help_shows_cdp_flag() {
    yoetz()
        .args(["browser", "attach", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--cdp"));
}

#[test]
fn browser_check_help_shows_cdp_flag() {
    yoetz()
        .args(["browser", "check", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--cdp"))
        .stdout(predicate::str::contains("--transport"));
}

#[test]
fn browser_recipe_help_shows_cdp_flag() {
    yoetz()
        .args(["browser", "recipe", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--cdp"));
}

#[test]
fn browser_recipe_help_shows_explicit_transport_flag() {
    yoetz()
        .args(["browser", "recipe", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--transport"))
        .stdout(predicate::str::contains("--allow-cdp-fallback"));
}

#[test]
fn browser_extension_help_shows_chatgpt_lifecycle() {
    yoetz()
        .args(["browser", "extension", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("install-host"))
        .stdout(predicate::str::contains("doctor"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("reconnect"))
        .stdout(predicate::str::contains("canary"));
}

#[test]
fn browser_extension_maintenance_help_shows_instance_selectors() {
    yoetz()
        .args(["browser", "extension", "reconnect", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--profile-email"))
        .stdout(predicate::str::contains("--extension-instance-id"))
        .stdout(predicate::str::contains("--extension-profile-id"));

    yoetz()
        .args(["browser", "extension", "canary", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--profile-email"))
        .stdout(predicate::str::contains("--extension-instance-id"))
        .stdout(predicate::str::contains("--extension-profile-id"));
}

#[test]
fn browser_extension_status_requires_chatgpt_scope() {
    yoetz()
        .args(["browser", "extension", "status", "--format", "json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("pass --chatgpt"));
}

#[test]
fn browser_extension_status_is_read_only_under_isolated_home() {
    let dir = tempfile::tempdir().unwrap();
    let native_hosts = dir.path().join("native-hosts");
    let state = dir.path().join("state");
    yoetz()
        .env("YOETZ_CHROME_NATIVE_MESSAGING_DIR", &native_hosts)
        .env("YOETZ_DIR", &state)
        .args([
            "--format",
            "json",
            "browser",
            "extension",
            "status",
            "--chatgpt",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"not_installed\""));
    assert!(!native_hosts.exists());
    assert!(!state.exists());
}

#[test]
fn browser_extension_status_text_is_copy_paste_friendly() {
    let dir = tempfile::tempdir().unwrap();
    let native_hosts = dir.path().join("native-hosts");
    let state = dir.path().join("state");
    yoetz()
        .env("YOETZ_CHROME_NATIVE_MESSAGING_DIR", &native_hosts)
        .env("YOETZ_DIR", &state)
        .args(["browser", "extension", "status", "--chatgpt"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Yoetz ChatGPT native extension: not_installed",
        ))
        .stdout(predicate::str::contains("extension_instance_id:"))
        .stdout(predicate::str::contains("connected_instances: none"));
}

#[test]
fn browser_attach_explicit_cdp_failure_does_not_fallback() {
    yoetz()
        .args(["browser", "attach", "--cdp", "http://127.0.0.1:1"])
        .env("YOETZ_DEV_BROWSER_BIN", "/definitely/missing")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "explicit --cdp failed; not falling back",
        ))
        .stderr(predicate::str::contains("could not attach to any Chrome instance").not());
}

#[test]
fn browser_login_explicit_cdp_failure_does_not_fallback() {
    let (_dir, bin) = fake_agent_browser_failure_bin("connectOverCDP blew up");

    yoetz()
        .args(["browser", "login", "--cdp", "http://127.0.0.1:9222"])
        .env("YOETZ_AGENT_BROWSER_BIN", &bin)
        .env("YOETZ_DEV_BROWSER_BIN", "/definitely/missing")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "explicit --cdp failed; not falling back",
        ))
        .stderr(predicate::str::contains("falling back to cookie sync").not())
        .stderr(predicate::str::contains("manual login").not());
}

#[test]
fn browser_check_explicit_cdp_failure_does_not_fallback() {
    yoetz()
        .args(["browser", "check", "--cdp", "http://127.0.0.1:1"])
        .env("YOETZ_DEV_BROWSER_BIN", "/definitely/missing")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "explicit --cdp failed; not falling back",
        ))
        .stderr(predicate::str::contains("browser profile not found").not());
}

#[test]
fn status_runs() {
    yoetz().arg("status").assert().success();
}

#[test]
fn invalid_subcommand() {
    yoetz()
        .arg("nonexistent")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));
}

#[test]
fn ask_em_dash_in_prompt_parses() {
    // Verifies that em-dashes in --prompt don't cause argument parsing errors.
    // The command will fail later (no API key / dry-run still needs provider),
    // but it must NOT fail with "unexpected argument '' found".
    let result = yoetz()
        .args([
            "ask",
            "--prompt",
            "summarize this \u{2014} and that",
            "--dry-run",
        ])
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("XAI_API_KEY")
        .assert();
    // Must not contain clap's argument parsing error
    result.stderr(predicate::str::contains("unexpected argument").not());
}

#[test]
fn bundle_em_dash_in_prompt_parses() {
    // Bundle with em-dash should parse without argument errors, even with a
    // prompt-only bundle.
    yoetz()
        .args(["bundle", "--prompt", "review \u{2014} all code"])
        .assert()
        .success()
        .stderr(predicate::str::contains("unexpected argument").not());
}

#[test]
fn bundle_prompt_file_without_files_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let prompt_file = dir.path().join("prompt.md");
    fs::write(&prompt_file, "Review this dossier").unwrap();

    yoetz()
        .args([
            "bundle",
            "--prompt-file",
            prompt_file.to_str().unwrap(),
            "--format",
            "json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"file_count\": 0"));
}

#[test]
fn bundle_markdown_separates_user_prompt_as_untrusted_input() {
    let output = yoetz()
        .args([
            "bundle",
            "--prompt",
            "Reply exactly AUTO_MODEL_OK.",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let bundle_md = json["artifacts"]["bundle_md"].as_str().unwrap();
    let markdown = fs::read_to_string(bundle_md).unwrap();
    assert!(markdown.contains("## User Prompt"));
    assert!(markdown.contains("untrusted user-supplied input"));
    assert!(markdown.contains("```text\nReply exactly AUTO_MODEL_OK.\n```"));
    assert!(!markdown.contains("## Prompt\n\nReply exactly AUTO_MODEL_OK."));
}

#[test]
fn bundle_all_includes_hidden_paths() {
    let dir = tempfile::tempdir().unwrap();
    let workflows = dir.path().join(".github/workflows");
    fs::create_dir_all(&workflows).unwrap();
    fs::write(workflows.join("ci.yml"), "name: CI\n").unwrap();

    yoetz()
        .current_dir(dir.path())
        .args([
            "bundle",
            "--prompt",
            "review repo",
            "--all",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(".github/workflows/ci.yml"));
}

#[test]
fn generate_video_openai_rejects_multiple_images() {
    let dir = tempfile::tempdir().unwrap();
    let image_a = dir.path().join("a.png");
    let image_b = dir.path().join("b.png");
    fs::write(&image_a, [0u8, 1, 2, 3]).unwrap();
    fs::write(&image_b, [0u8, 1, 2, 3]).unwrap();

    yoetz()
        .args([
            "generate",
            "video",
            "--provider",
            "openai",
            "--model",
            "sora-1",
            "--prompt",
            "animate this",
            "--image",
            image_a.to_str().unwrap(),
            "--image",
            image_b.to_str().unwrap(),
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "provider openai accepts at most one --image for video generation",
        ));
}

#[test]
fn council_em_dash_in_prompt_parses() {
    let result = yoetz()
        .args([
            "council",
            "--prompt",
            "compare \u{2014} contrast",
            "--dry-run",
            "--models",
            "test/model",
        ])
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("XAI_API_KEY")
        .assert();
    result.stderr(predicate::str::contains("unexpected argument").not());
}

#[test]
fn models_frontier_help_shows_flags() {
    yoetz()
        .args(["models", "frontier", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--family"))
        .stdout(predicate::str::contains("--all"));
}

#[test]
fn models_frontier_default_output_is_curated() {
    let fixture = frontier_fixture();
    yoetz_with_frontier_fixture(&fixture)
        .args(["models", "frontier"])
        .assert()
        .success()
        .stdout(predicate::str::contains("FAMILY"))
        .stdout(predicate::str::contains("MODEL"))
        .stdout(predicate::str::contains("openai/gpt-5.4-pro"))
        .stdout(predicate::str::contains("reseller/one-off-model").not());
}

#[test]
fn models_frontier_family_filter_limits_output() {
    let fixture = frontier_fixture();
    yoetz_with_frontier_fixture(&fixture)
        .args(["models", "frontier", "--family", "openai"])
        .assert()
        .success()
        .stdout(predicate::str::contains("openai/gpt-5.4-pro"))
        .stdout(predicate::str::contains("anthropic/claude-opus-4-6").not())
        .stdout(predicate::str::contains("reseller/one-off-model").not());
}

#[test]
fn models_frontier_all_shows_every_family() {
    let fixture = frontier_fixture();
    yoetz_with_frontier_fixture(&fixture)
        .args(["models", "frontier", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("openai/gpt-5.4-pro"))
        .stdout(predicate::str::contains("anthropic/claude-opus-4-6"))
        .stdout(predicate::str::contains("reseller/one-off-model"));
}

#[test]
fn allow_unknown_flag_accepted() {
    yoetz()
        .args(["--allow-unknown", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--allow-unknown"));
}
