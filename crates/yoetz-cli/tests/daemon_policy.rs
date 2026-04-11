use std::fs;
use std::path::PathBuf;

fn source_file(path: &str) -> String {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(manifest_dir.join(path)).expect("read source file")
}

fn occurrences(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

#[test]
fn main_keeps_destructive_recovery_scoped_to_browser_reset() {
    let main_rs = source_file("src/main.rs");

    assert!(main_rs.contains("BrowserCommand::Reset(_) => {"));
    assert_eq!(
        occurrences(&main_rs, "browser::force_kill_stale_daemon();"),
        1,
        "force_kill_stale_daemon should only run in BrowserCommand::Reset"
    );
    assert_eq!(
        occurrences(&main_rs, "dev_browser::stop_daemon()?"),
        1,
        "dev-browser stop_daemon should only run in BrowserCommand::Reset"
    );
    assert_eq!(
        occurrences(&main_rs, "browser::close_browser()?;"),
        1,
        "legacy managed daemon close should only run in BrowserCommand::Reset"
    );
    assert_eq!(
        occurrences(&main_rs, "browser::close_live_attach_session()?;"),
        1,
        "live-attach session close should only run in BrowserCommand::Reset"
    );
}

#[test]
fn browser_keeps_destructive_helpers_in_whitelisted_locations() {
    let browser_rs = source_file("src/browser.rs");

    assert_eq!(
        occurrences(
            &browser_rs,
            "pub fn force_kill_stale_daemon() -> DaemonRecoveryAction {"
        ),
        1
    );
    assert_eq!(
        occurrences(&browser_rs, "force_kill_stale_daemon("),
        1,
        "force_kill_stale_daemon should not be invoked from normal browser flow helpers"
    );
    assert_eq!(
        occurrences(&browser_rs, "close_browser_daemon()"),
        3,
        "close_browser_daemon should only appear in close_browser, close_browser_for_connection, and its own definition"
    );
    assert!(
        browser_rs.contains(
            "pub fn close_browser_for_connection(connection: &BrowserConnection) -> Result<()> {\n    if connection.is_live_attach() {\n        return close_live_attach_session();\n    }\n    close_browser_daemon()\n}"
        ),
        "live-attach close path must stay non-destructive"
    );
    assert!(
        browser_rs.contains("if !connection.is_live_attach() {\n        let _ = close_browser_for_connection(connection);\n    }"),
        "normal auth checks must only close managed non-live daemons"
    );
}

#[test]
fn dev_browser_stop_daemon_stays_helper_only() {
    let dev_browser_rs = source_file("src/dev_browser.rs");

    assert_eq!(
        occurrences(&dev_browser_rs, "pub fn stop_daemon() -> Result<bool> {"),
        1
    );
    assert_eq!(
        occurrences(&dev_browser_rs, "stop_daemon("),
        1,
        "dev-browser stop_daemon should remain a helper, not a normal-flow callsite"
    );
}
