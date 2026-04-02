use anyhow::{anyhow, Context, Result};
use chromiumoxide::Browser;
use clap::{Parser, Subcommand, ValueEnum};
use futures::StreamExt;
use rmcp::{
    model::{CallToolRequestParams, CallToolResult},
    transport::{ConfigureCommandExt, TokioChildProcess},
    ServiceExt,
};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_TARGET_URL: &str = "https://chatgpt.com/";
const AUTO_CONNECT_LOG_FILE: &str = "/tmp/chrome-devtools-mcp-auto-connect.log";
const WS_ENDPOINT_LOG_FILE: &str = "/tmp/chrome-devtools-mcp-ws-endpoint.log";

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    ChromiumoxideNavigateExistingTab {
        #[arg(long)]
        ws_endpoint: Option<String>,
        #[arg(long, default_value = DEFAULT_TARGET_URL)]
        target_url: String,
    },
    McpNavigatePage {
        #[arg(long, value_enum, default_value_t = McpMode::AutoConnect)]
        mode: McpMode,
        #[arg(long)]
        ws_endpoint: Option<String>,
        #[arg(long, default_value = DEFAULT_TARGET_URL)]
        target_url: String,
        #[arg(long)]
        log_file: Option<PathBuf>,
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, ValueEnum)]
enum McpMode {
    AutoConnect,
    WsEndpoint,
}

#[derive(Debug, Serialize)]
struct TabSummary {
    target_id: String,
    url: Option<String>,
    title: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChromiumoxideSummary {
    backend: &'static str,
    ws_endpoint: String,
    fetched_target_count: usize,
    existing_tabs: Vec<TabSummary>,
    selected_target_id: String,
    selection_reason: &'static str,
    target_url: String,
    final_url: Option<String>,
    final_title: Option<String>,
}

#[derive(Debug, Serialize)]
struct McpSummary {
    backend: &'static str,
    mode: McpMode,
    ws_endpoint: Option<String>,
    target_url: String,
    tool_names: Vec<String>,
    list_pages: Outcome,
    select_page: Outcome,
    navigate_page: Outcome,
    log_file: Option<String>,
    log_tail: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Outcome {
    Skipped { reason: String },
    Ok { data: Value },
    Error { error: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::ChromiumoxideNavigateExistingTab {
            ws_endpoint,
            target_url,
        } => run_chromiumoxide_navigate_existing_tab(ws_endpoint, &target_url).await,
        Command::McpNavigatePage {
            mode,
            ws_endpoint,
            target_url,
            log_file,
        } => run_mcp_navigate_page(mode, ws_endpoint, &target_url, log_file).await,
    }
}

async fn run_chromiumoxide_navigate_existing_tab(
    ws_endpoint: Option<String>,
    target_url: &str,
) -> Result<()> {
    let ws_endpoint = ws_endpoint.unwrap_or(read_devtools_active_port_ws()?);
    let connect_result =
        tokio::time::timeout(Duration::from_secs(20), Browser::connect(&ws_endpoint))
            .await
            .context("chromiumoxide Browser::connect timed out")?;
    let (mut browser, mut handler) = connect_result
        .with_context(|| format!("chromiumoxide Browser::connect failed for {ws_endpoint}"))?;

    let handler_task = tokio::spawn(async move {
        while let Some(event) = handler.next().await {
            event?;
        }
        anyhow::Ok(())
    });

    let fetched_targets = browser
        .fetch_targets()
        .await
        .context("chromiumoxide fetch_targets failed")?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let pages = browser
        .pages()
        .await
        .context("chromiumoxide pages() failed")?;
    if pages.is_empty() {
        handler_task.abort();
        return Err(anyhow!(
            "chromiumoxide connected but found no existing tabs"
        ));
    }

    let mut existing_tabs = Vec::new();
    let mut preferred_index = None;
    let mut selection_reason = "first_existing_tab";
    for (idx, page) in pages.iter().enumerate() {
        let url = page.url().await.unwrap_or(None);
        let title = page.get_title().await.unwrap_or(None);
        if preferred_index.is_none() && url_is_safe_blank(url.as_deref()) {
            preferred_index = Some(idx);
            selection_reason = "preferred_blank_tab";
        }
        existing_tabs.push(TabSummary {
            target_id: page.target_id().as_ref().to_string(),
            url,
            title,
        });
    }

    let selected_index = preferred_index.unwrap_or(0);
    let selected_page = &pages[selected_index];
    let selected_target_id = selected_page.target_id().as_ref().to_string();
    selected_page
        .bring_to_front()
        .await
        .context("chromiumoxide bring_to_front failed")?;
    tokio::time::timeout(Duration::from_secs(30), selected_page.goto(target_url))
        .await
        .context("chromiumoxide goto timed out")?
        .with_context(|| format!("chromiumoxide goto failed for {target_url}"))?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let summary = ChromiumoxideSummary {
        backend: "chromiumoxide",
        ws_endpoint,
        fetched_target_count: fetched_targets.len(),
        existing_tabs,
        selected_target_id,
        selection_reason,
        target_url: target_url.to_string(),
        final_url: selected_page.url().await.unwrap_or(None),
        final_title: selected_page.get_title().await.unwrap_or(None),
    };
    print_json(&summary)?;

    drop(browser);
    handler_task.abort();
    Ok(())
}

async fn run_mcp_navigate_page(
    mode: McpMode,
    ws_endpoint: Option<String>,
    target_url: &str,
    log_file: Option<PathBuf>,
) -> Result<()> {
    let log_file = log_file.unwrap_or_else(|| default_mcp_log_file(mode));
    let _ = fs::remove_file(&log_file);

    let resolved_ws = match mode {
        McpMode::AutoConnect => None,
        McpMode::WsEndpoint => Some(ws_endpoint.unwrap_or(read_devtools_active_port_ws()?)),
    };
    let (client, resolved_ws) = connect_mcp(mode, resolved_ws, &log_file).await?;
    let tools = client.list_all_tools().await?;
    let tool_names = tools
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect::<Vec<_>>();

    let list_pages = outcome_from_tool(
        call_tool_timed(&client, "list_pages", json_object(json!({}))?, 10).await,
    );

    let selected_page_id = extract_first_page_id_from_outcome(&list_pages);
    let select_page = if let Some(page_id) = selected_page_id {
        outcome_from_tool(
            call_tool_timed(
                &client,
                "select_page",
                json_object(json!({"pageId": page_id, "bringToFront": true}))?,
                10,
            )
            .await,
        )
    } else {
        Outcome::Skipped {
            reason: "list_pages did not produce a page id".to_string(),
        }
    };

    let navigate_page = outcome_from_tool(
        call_tool_timed(
            &client,
            "navigate_page",
            json_object(json!({
                "type": "url",
                "url": target_url,
                "timeout": 20000
            }))?,
            25,
        )
        .await,
    );

    let summary = McpSummary {
        backend: "chrome-devtools-mcp",
        mode,
        ws_endpoint: resolved_ws,
        target_url: target_url.to_string(),
        tool_names,
        list_pages,
        select_page,
        navigate_page,
        log_file: Some(log_file.display().to_string()),
        log_tail: read_tail(&log_file, 80).ok(),
    };
    print_json(&summary)?;

    client.cancel().await?;
    Ok(())
}

async fn connect_mcp(
    mode: McpMode,
    ws_endpoint: Option<String>,
    log_file: &Path,
) -> Result<(
    rmcp::service::RunningService<rmcp::RoleClient, ()>,
    Option<String>,
)> {
    let mut args = vec!["-y".to_string(), "chrome-devtools-mcp@0.21.0".to_string()];
    match mode {
        McpMode::AutoConnect => args.push("--autoConnect".to_string()),
        McpMode::WsEndpoint => {
            let ws = ws_endpoint
                .clone()
                .ok_or_else(|| anyhow!("ws-endpoint mode requires a websocket endpoint"))?;
            args.push("--wsEndpoint".to_string());
            args.push(ws);
        }
    }
    args.push("--logFile".to_string());
    args.push(log_file.display().to_string());
    args.push("--no-usage-statistics".to_string());
    args.push("--no-performance-crux".to_string());

    let transport =
        TokioChildProcess::new(tokio::process::Command::new("npx").configure(move |cmd| {
            cmd.args(&args);
        }))
        .context("failed to spawn chrome-devtools-mcp via npx")?;
    let client = ().serve(transport).await?;
    Ok((client, ws_endpoint))
}

async fn call_tool_timed(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &str,
    arguments: Map<String, Value>,
    timeout_secs: u64,
) -> Result<CallToolResult> {
    tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        call_tool(client, name, arguments),
    )
    .await
    .with_context(|| format!("tool `{name}` timed out after {timeout_secs}s"))?
}

async fn call_tool(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &str,
    arguments: Map<String, Value>,
) -> Result<CallToolResult> {
    Ok(client
        .call_tool(CallToolRequestParams::new(name.to_string()).with_arguments(arguments))
        .await?)
}

fn outcome_from_tool(result: Result<CallToolResult>) -> Outcome {
    match result {
        Ok(result) => Outcome::Ok {
            data: result_to_json(&result),
        },
        Err(error) => Outcome::Error {
            error: format!("{error:#}"),
        },
    }
}

fn extract_first_page_id_from_outcome(outcome: &Outcome) -> Option<u64> {
    let Outcome::Ok { data } = outcome else {
        return None;
    };

    if let Some(page_id) = data
        .pointer("/structuredContent/0/id")
        .and_then(Value::as_u64)
    {
        return Some(page_id);
    }
    if let Some(page_id) = data
        .pointer("/structuredContent/0/pageId")
        .and_then(Value::as_u64)
    {
        return Some(page_id);
    }
    if let Some(page_id) = data
        .pointer("/structuredContent/pages/0/id")
        .and_then(Value::as_u64)
    {
        return Some(page_id);
    }
    None
}

fn read_devtools_active_port_ws() -> Result<String> {
    let candidates = [
        PathBuf::from(
            "~/Library/Application Support/Google/Chrome/DevToolsActivePort"
                .replace('~', &home_dir_string()),
        ),
        PathBuf::from(
            "~/Library/Application Support/Google/Chrome for Testing/DevToolsActivePort"
                .replace('~', &home_dir_string()),
        ),
    ];

    for candidate in candidates {
        if candidate.is_file() {
            let content = fs::read_to_string(&candidate)
                .with_context(|| format!("read {}", candidate.display()))?;
            let mut lines = content.lines();
            let port = lines
                .next()
                .ok_or_else(|| anyhow!("missing port in {}", candidate.display()))?;
            let path = lines
                .next()
                .ok_or_else(|| anyhow!("missing websocket path in {}", candidate.display()))?;
            return Ok(format!("ws://127.0.0.1:{port}{path}"));
        }
    }

    Err(anyhow!(
        "could not find DevToolsActivePort in default Chrome locations"
    ))
}

fn home_dir_string() -> String {
    std::env::var("HOME").unwrap_or_else(|_| ".".to_string())
}

fn default_mcp_log_file(mode: McpMode) -> PathBuf {
    match mode {
        McpMode::AutoConnect => PathBuf::from(AUTO_CONNECT_LOG_FILE),
        McpMode::WsEndpoint => PathBuf::from(WS_ENDPOINT_LOG_FILE),
    }
}

fn json_object(value: Value) -> Result<Map<String, Value>> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("expected JSON object arguments"))
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn result_to_json(result: &CallToolResult) -> Value {
    json!({
        "structuredContent": result.structured_content,
        "content": result.content,
        "isError": result.is_error,
    })
}

fn read_tail(path: &Path, max_lines: usize) -> Result<String> {
    let content = fs::read_to_string(path)?;
    let lines = content.lines().collect::<Vec<_>>();
    let start = lines.len().saturating_sub(max_lines);
    Ok(lines[start..].join("\n"))
}

fn url_is_safe_blank(url: Option<&str>) -> bool {
    matches!(
        url,
        Some("about:blank")
            | Some("chrome://newtab/")
            | Some("chrome://new-tab-page/")
            | Some("chrome-search://local-ntp/local-ntp.html")
    )
}
