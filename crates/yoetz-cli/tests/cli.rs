use assert_cmd::Command;
use predicates::prelude::*;

fn yoetz() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("yoetz").unwrap()
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
        .stdout(predicate::str::contains("--files"));
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
    // Bundle with em-dash should parse without argument errors.
    // --all is required to avoid "files required" error.
    let result = yoetz()
        .args(["bundle", "--prompt", "review \u{2014} all code", "--all"])
        .assert();
    result.stderr(predicate::str::contains("unexpected argument").not());
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
