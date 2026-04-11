//! rmcp client wrapper over `chrome-devtools-mcp` (stdio MCP server).

use anyhow::{anyhow, bail, Context, Result};
use reqwest::Url;
use rmcp::{
    model::{CallToolRequestParams, CallToolResult, Content, RawContent, ResourceContents},
    service::{RoleClient, RunningService},
    transport::TokioChildProcess,
    ServiceExt,
};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

type CdpService = RunningService<RoleClient, ()>;

pub struct CdpMcpClient {
    service: Mutex<Option<CdpService>>,
}

#[derive(Debug, Clone)]
pub struct NewPageResult {
    pub page_id: String,
}

#[derive(Debug, Clone)]
pub struct NavigateResult {
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub raw: Value,
}

impl Snapshot {
    pub fn find_uid_by_role(&self, role: &str, name: &str) -> Option<String> {
        let wanted_role = role.trim();
        let wanted_name = name.trim();
        walk_snapshot(&self.raw, &mut |node| {
            let node_role = node.get("role").and_then(Value::as_str)?;
            if node_role != wanted_role {
                return None;
            }

            if !wanted_name.is_empty() {
                let node_name = node.get("name").and_then(Value::as_str)?;
                if node_name != wanted_name {
                    return None;
                }
            }

            node.get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
    }

    pub fn find_uid_by_text(&self, text: &str) -> Option<String> {
        let wanted = text.trim().to_ascii_lowercase();
        if wanted.is_empty() {
            return None;
        }

        walk_snapshot(&self.raw, &mut |node| {
            if node_contains_text(node, &wanted) {
                node.get("id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            } else {
                None
            }
        })
    }

    pub fn count_by_role(&self, role: &str) -> usize {
        count_nodes(&self.raw, role)
    }
}

#[derive(Debug, Clone)]
pub struct WaitForResult {
    pub matched_text: String,
}

/// Locate the `chrome-devtools-mcp` invocation to spawn.
///
/// Preference order:
/// 1. The globally installed `chrome-devtools-mcp` binary on `PATH` (from
///    `npm install -g chrome-devtools-mcp`). Fastest path, version-pinned
///    at install time, no per-invocation network fetch.
/// 2. `npx -y chrome-devtools-mcp@latest` as the transparent fallback for
///    users who haven't installed it globally yet. Slower first run (npx
///    fetches the package) but zero-setup on a fresh machine.
fn resolve_chrome_devtools_mcp_command() -> Command {
    if let Some(path) = which_chrome_devtools_mcp() {
        let mut command = Command::new(path);
        command.arg("--experimentalStructuredContent");
        return command;
    }

    let mut command = Command::new("npx");
    command
        .arg("-y")
        .arg("chrome-devtools-mcp@latest")
        .arg("--experimentalStructuredContent");
    command
}

/// Look up `chrome-devtools-mcp` on `PATH` using the same rules as the shell.
fn which_chrome_devtools_mcp() -> Option<std::path::PathBuf> {
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join("chrome-devtools-mcp");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

impl CdpMcpClient {
    pub async fn connect_to_running_chrome(cdp_endpoint: Option<&str>) -> Result<Self> {
        let endpoint = cdp_endpoint.unwrap_or("http://127.0.0.1:9222");
        let parsed = Url::parse(endpoint)
            .with_context(|| format!("invalid Chrome CDP endpoint `{endpoint}`"))?;

        let mut command = resolve_chrome_devtools_mcp_command();
        command
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());

        match parsed.scheme() {
            "ws" | "wss" => {
                command.arg(format!("--wsEndpoint={endpoint}"));
            }
            _ => {
                command.arg(format!("--browser-url={endpoint}"));
            }
        }

        let (transport, stderr) = TokioChildProcess::builder(command)
            .stderr(Stdio::piped())
            .spawn()
            .context(
                "failed to start chrome-devtools-mcp; ensure it is installed via `npm install -g chrome-devtools-mcp`",
            )?;

        if let Some(stderr) = stderr {
            let verbose = std::env::var_os("YOETZ_BROWSER_DEBUG").is_some();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if verbose && !line.trim().is_empty() {
                        eprintln!("chrome-devtools-mcp: {line}");
                    }
                }
            });
        }

        let service = ().serve(transport).await.with_context(|| {
            format!("handshaking with chrome-devtools-mcp at `{endpoint}` failed")
        })?;

        Ok(Self {
            service: Mutex::new(Some(service)),
        })
    }

    pub async fn new_page(
        &self,
        url: &str,
        background: bool,
        timeout_ms: u64,
    ) -> Result<NewPageResult> {
        let result = self
            .call_tool(
                "new_page",
                json!({
                    "url": url,
                    "background": background,
                    "timeout": timeout_ms,
                }),
            )
            .await?;

        let page = selected_page_entry(&result).context(
            "chrome-devtools-mcp `new_page` did not return a selected page in structuredContent.pages",
        )?;
        let page_id = page
            .get("id")
            .and_then(Value::as_i64)
            .context("chrome-devtools-mcp `new_page` selected page is missing numeric `id`")?;

        Ok(NewPageResult {
            page_id: page_id.to_string(),
        })
    }

    pub async fn navigate_page(&self, url: &str, timeout_ms: u64) -> Result<NavigateResult> {
        self.call_tool(
            "navigate_page",
            json!({
                "type": "url",
                "url": url,
                "timeout": timeout_ms,
            }),
        )
        .await?;

        Ok(NavigateResult {
            url: url.to_owned(),
        })
    }

    pub async fn take_snapshot(&self, verbose: bool) -> Result<Snapshot> {
        let result = self
            .call_tool("take_snapshot", json!({ "verbose": verbose }))
            .await?;

        let structured = structured_content(&result, "take_snapshot")?;
        let snapshot = structured.get("snapshot").cloned().context(
            "chrome-devtools-mcp `take_snapshot` response is missing structuredContent.snapshot",
        )?;

        Ok(Snapshot { raw: snapshot })
    }

    pub async fn click(&self, uid: &str, double_click: bool) -> Result<()> {
        self.call_tool(
            "click",
            json!({
                "uid": uid,
                "dblClick": double_click,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn type_text(&self, text: &str, submit_key: Option<&str>) -> Result<()> {
        let mut arguments = json!({
            "text": text,
        });
        if let Some(submit_key) = submit_key {
            arguments["submitKey"] = Value::String(submit_key.to_owned());
        }

        self.call_tool("type_text", arguments).await?;
        Ok(())
    }

    pub async fn upload_file(&self, uid: &str, file_path: &Path) -> Result<()> {
        let file_path = file_path.to_str().ok_or_else(|| {
            anyhow!(
                "upload_file path is not valid UTF-8: {}",
                file_path.display()
            )
        })?;

        self.call_tool(
            "upload_file",
            json!({
                "uid": uid,
                "filePath": file_path,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn wait_for(&self, text: &[&str], timeout_ms: u64) -> Result<WaitForResult> {
        if text.is_empty() {
            bail!("wait_for requires at least one text hint");
        }

        self.call_tool(
            "wait_for",
            json!({
                "text": text,
                "timeout": timeout_ms,
            }),
        )
        .await?;

        Ok(WaitForResult {
            matched_text: text[0].to_owned(),
        })
    }

    pub async fn evaluate_script(
        &self,
        function: &str,
        args: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let uid_args = normalize_uid_args(args)?;
        let result = self
            .call_tool(
                "evaluate_script",
                json!({
                    "function": function,
                    "args": uid_args,
                }),
            )
            .await?;

        let text = join_text_content(&result);
        parse_evaluate_script_json(&text)
            .with_context(|| format!("failed to parse `evaluate_script` JSON response:\n{text}"))
    }

    async fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<CallToolResult> {
        let params = CallToolRequestParams::new(tool_name.to_owned())
            .with_arguments(value_to_object(arguments, tool_name)?);

        let guard = self.service.lock().await;
        let service = guard
            .as_ref()
            .context("chrome-devtools-mcp client has already been shut down")?;
        let result = service
            .peer()
            .call_tool(params)
            .await
            .with_context(|| format!("chrome-devtools-mcp `{tool_name}` call failed"))?;

        if result.is_error.unwrap_or(false) {
            bail!(
                "chrome-devtools-mcp `{tool_name}` failed: {}",
                tool_failure_text(&result)
            );
        }

        Ok(result)
    }
}

impl Drop for CdpMcpClient {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.service.try_lock() {
            if let Some(service) = guard.take() {
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        let _ = service.cancel().await;
                    });
                } else {
                    drop(service);
                }
            }
        }
    }
}

fn value_to_object(arguments: Value, tool_name: &str) -> Result<rmcp::model::JsonObject> {
    match arguments {
        Value::Object(map) => Ok(map),
        other => {
            bail!("chrome-devtools-mcp `{tool_name}` arguments must be a JSON object, got {other}")
        }
    }
}

fn structured_content<'a>(result: &'a CallToolResult, tool_name: &str) -> Result<&'a Value> {
    result.structured_content.as_ref().ok_or_else(|| {
        anyhow!("chrome-devtools-mcp `{tool_name}` did not return structuredContent")
    })
}

fn selected_page_entry(result: &CallToolResult) -> Option<&Value> {
    let pages = result
        .structured_content
        .as_ref()?
        .get("pages")?
        .as_array()?;
    pages
        .iter()
        .find(|page| page.get("selected").and_then(Value::as_bool) == Some(true))
        .or_else(|| pages.last())
}

fn join_text_content(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(content_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_failure_text(result: &CallToolResult) -> String {
    let mut parts = Vec::new();
    let text = join_text_content(result);
    if !text.trim().is_empty() {
        parts.push(text);
    }
    if let Some(structured) = &result.structured_content {
        let structured = structured.to_string();
        if !structured.is_empty() {
            parts.push(structured);
        }
    }
    if parts.is_empty() {
        "tool returned an error with no message".to_owned()
    } else {
        parts.join("\n")
    }
}

fn content_text(content: &Content) -> Option<String> {
    match &content.raw {
        RawContent::Text(text) => Some(text.text.clone()),
        RawContent::Resource(resource) => match &resource.resource {
            ResourceContents::TextResourceContents { text, .. } => Some(text.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn normalize_uid_args(args: Vec<Value>) -> Result<Vec<String>> {
    args.into_iter()
        .enumerate()
        .map(|(idx, value)| match value {
            Value::String(uid) => Ok(uid),
            other => {
                bail!("evaluate_script args must be snapshot uid strings; arg {idx} was {other}")
            }
        })
        .collect()
}

fn parse_evaluate_script_json(text: &str) -> Result<Value> {
    if let Some(fenced) = extract_json_fence(text) {
        return serde_json::from_str(&fenced)
            .with_context(|| format!("invalid JSON inside evaluate_script fence: {fenced}"));
    }

    let trimmed = text.trim();
    if trimmed.is_empty() {
        bail!("evaluate_script returned no text content");
    }

    serde_json::from_str(trimmed).with_context(|| {
        "evaluate_script response did not contain a fenced ```json block or raw JSON payload"
    })
}

fn extract_json_fence(text: &str) -> Option<String> {
    let mut in_fence = false;
    let mut body = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if !in_fence {
            if trimmed.eq_ignore_ascii_case("```json") || trimmed == "```" {
                in_fence = true;
            }
            continue;
        }

        if trimmed == "```" {
            return Some(body.join("\n"));
        }

        body.push(line);
    }

    None
}

fn walk_snapshot<T, F>(node: &Value, visit: &mut F) -> Option<T>
where
    F: FnMut(&serde_json::Map<String, Value>) -> Option<T>,
{
    let object = node.as_object()?;
    if let Some(found) = visit(object) {
        return Some(found);
    }

    if let Some(children) = object.get("children").and_then(Value::as_array) {
        for child in children {
            if let Some(found) = walk_snapshot(child, visit) {
                return Some(found);
            }
        }
    }

    None
}

fn count_nodes(node: &Value, role: &str) -> usize {
    let Some(object) = node.as_object() else {
        return 0;
    };

    let mut count = usize::from(object.get("role").and_then(Value::as_str) == Some(role));
    if let Some(children) = object.get("children").and_then(Value::as_array) {
        count += children
            .iter()
            .map(|child| count_nodes(child, role))
            .sum::<usize>();
    }
    count
}

fn node_contains_text(node: &serde_json::Map<String, Value>, wanted: &str) -> bool {
    node.iter().any(|(key, value)| match value {
        Value::String(text) => {
            key != "id" && key != "role" && text.to_ascii_lowercase().contains(wanted)
        }
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::AnnotateAble;

    fn snapshot_fixture() -> Snapshot {
        Snapshot {
            raw: json!({
                "id": "root",
                "role": "root_web_area",
                "name": "ChatGPT",
                "children": [
                    {
                        "id": "composer",
                        "role": "textbox",
                        "name": "Message ChatGPT"
                    },
                    {
                        "id": "upload",
                        "role": "button",
                        "name": "Upload files and more"
                    },
                    {
                        "id": "assistant-list",
                        "role": "list",
                        "children": [
                            {
                                "id": "assistant-1",
                                "role": "listitem",
                                "name": "First answer"
                            },
                            {
                                "id": "assistant-2",
                                "role": "listitem",
                                "description": "Regenerate"
                            }
                        ]
                    }
                ]
            }),
        }
    }

    #[test]
    fn snapshot_find_uid_by_role_uses_structured_id() {
        let snapshot = snapshot_fixture();
        assert_eq!(
            snapshot.find_uid_by_role("textbox", "Message ChatGPT"),
            Some("composer".to_owned())
        );
        assert_eq!(
            snapshot.find_uid_by_role("button", "Upload files and more"),
            Some("upload".to_owned())
        );
    }

    #[test]
    fn snapshot_find_uid_by_text_searches_all_string_fields() {
        let snapshot = snapshot_fixture();
        assert_eq!(
            snapshot.find_uid_by_text("regenerate"),
            Some("assistant-2".to_owned())
        );
        assert_eq!(
            snapshot.find_uid_by_text("first answer"),
            Some("assistant-1".to_owned())
        );
    }

    #[test]
    fn snapshot_count_by_role_counts_nested_nodes() {
        let snapshot = snapshot_fixture();
        assert_eq!(snapshot.count_by_role("listitem"), 2);
        assert_eq!(snapshot.count_by_role("button"), 1);
    }

    #[test]
    fn evaluate_script_parser_reads_fenced_json() {
        let value =
            parse_evaluate_script_json("Script ran on page and returned:\n```json\n\"hello\"\n```")
                .expect("should parse fenced json");
        assert_eq!(value, Value::String("hello".to_owned()));
    }

    #[test]
    fn evaluate_script_args_must_be_uid_strings() {
        let err = normalize_uid_args(vec![Value::String("uid-1".to_owned()), json!(1)])
            .expect_err("non-string args should be rejected");
        assert!(err.to_string().contains("snapshot uid strings"));
    }

    #[test]
    fn selected_page_parses_structured_pages() {
        let mut result = CallToolResult::success(vec![]);
        result.structured_content = Some(json!({
            "pages": [
                {"id": 4, "url": "https://example.com", "selected": false},
                {"id": 7, "url": "https://chatgpt.com", "selected": true}
            ]
        }));

        let page = selected_page_entry(&result).expect("selected page should exist");
        assert_eq!(page.get("id").and_then(Value::as_i64), Some(7));
    }

    #[test]
    fn call_tool_result_error_text_includes_text_and_structured_content() {
        let mut result = CallToolResult::error(vec![RawContent::text("boom").no_annotation()]);
        result.structured_content = Some(json!({"message": "bad"}));

        let detail = tool_failure_text(&result);
        assert!(detail.contains("boom"));
        assert!(detail.contains("\"message\":\"bad\""));
    }

    #[test]
    fn value_to_object_rejects_non_objects() {
        let err = value_to_object(json!(["bad"]), "take_snapshot")
            .expect_err("non-object args should fail");
        assert!(err.to_string().contains("must be a JSON object"));
    }

    #[test]
    fn evaluate_script_parser_rejects_non_json_text() {
        let err = parse_evaluate_script_json("Script ran on page and returned:\nnot json")
            .expect_err("raw non-json text should fail");
        assert!(err.to_string().contains("fenced ```json block"));
    }

    #[test]
    fn resource_text_content_is_extracted() {
        let content = RawContent::embedded_text("memory://note", "hello").no_annotation();
        assert_eq!(content_text(&content), Some("hello".to_owned()));
    }

    #[test]
    fn rmcp_object_helper_stays_compatible_with_empty_object_calls() {
        let args = rmcp::model::object(json!({}));
        assert!(args.is_empty());
    }
}
