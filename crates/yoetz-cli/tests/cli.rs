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
