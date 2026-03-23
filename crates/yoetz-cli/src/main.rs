use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose, Engine as _};
use clap::{Args, Parser, Subcommand};
use jsonschema::Validator;
use litellm_rust::{
    ChatContentPart, ChatContentPartFile, ChatContentPartImageUrl, ChatContentPartText, ChatFile,
    ChatImageUrl, ChatMessageContent, ChatRequest, ImageData, LiteLLM,
    ProviderConfig as LiteProviderConfig, ProviderKind as LiteProviderKind,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

mod browser;
mod budget;
mod commands;
mod fuzzy;
mod http;
mod providers;
mod registry;

use yoetz_core::config::Config;
use yoetz_core::media::{MediaInput, MediaType};
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};
use yoetz_core::registry::ModelRegistry;
use yoetz_core::session::{list_sessions, write_json as write_json_file};
use yoetz_core::types::{ArtifactPaths, PricingEstimate, Usage};

use http::send_json;

/// Cap for registry-derived max_output_tokens. Generous enough for reasoning models
/// (which consume thinking tokens from the budget) but prevents runaway costs on
/// simple queries when no explicit --max-output-tokens is provided.
const REGISTRY_OUTPUT_TOKENS_CAP: usize = 16384;

#[derive(Parser)]
#[command(
    name = "yoetz",
    version,
    about = "Fast, agent-friendly LLM council tool"
)]
struct Cli {
    #[arg(long, global = true)]
    format: Option<String>,

    #[arg(long, global = true)]
    debug: bool,

    #[arg(long, global = true)]
    output_final: Option<PathBuf>,

    #[arg(long, global = true)]
    output_schema: Option<PathBuf>,

    #[arg(long, global = true)]
    profile: Option<String>,

    #[arg(long, global = true, default_value = "60")]
    timeout_secs: u64,

    /// Allow unrecognized model IDs (for self-hosted models not in the registry)
    #[arg(long, global = true)]
    allow_unknown: bool,

    #[command(subcommand)]
    command: Commands,
}

struct AppContext {
    config: Config,
    client: reqwest::Client,
    litellm: std::sync::Arc<LiteLLM>,
    output_final: Option<PathBuf>,
    output_schema: Option<PathBuf>,
    debug: bool,
    allow_unknown: bool,
}

#[derive(Subcommand)]
enum Commands {
    Ask(AskArgs),
    Bundle(BundleArgs),
    Status,
    Session(SessionArgs),
    Models(ModelsArgs),
    Pricing(PricingArgs),
    Browser(BrowserArgs),
    Council(CouncilArgs),
    Review(ReviewArgs),
    Apply(ApplyArgs),
    Generate(GenerateArgs),
}

#[derive(Args)]
struct AskArgs {
    #[arg(short, long, allow_hyphen_values = true)]
    prompt: Option<String>,

    #[arg(long)]
    prompt_file: Option<PathBuf>,

    #[arg(long, short = 'f')]
    files: Vec<String>,

    #[arg(long)]
    exclude: Vec<String>,

    #[arg(long, default_value = "200000")]
    max_file_bytes: usize,

    #[arg(long, default_value = "5000000")]
    max_total_bytes: usize,

    #[arg(long)]
    provider: Option<String>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long, default_value = "0.1")]
    temperature: f32,

    #[arg(long)]
    max_output_tokens: Option<usize>,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    max_cost_usd: Option<f64>,

    #[arg(long)]
    daily_budget_usd: Option<f64>,

    #[arg(long, value_name = "PATH_OR_URL")]
    image: Vec<String>,

    #[arg(
        long,
        value_name = "MIME",
        help = "Override MIME type for --image inputs (1 value or 1 per image)"
    )]
    image_mime: Vec<String>,

    #[arg(long, value_name = "PATH_OR_URL")]
    video: Option<String>,

    #[arg(
        long,
        value_name = "MIME",
        help = "Override MIME type for --video input"
    )]
    video_mime: Option<String>,

    #[arg(long, value_name = "json|text")]
    response_format: Option<String>,

    #[arg(long)]
    response_schema: Option<PathBuf>,

    #[arg(long)]
    response_schema_name: Option<String>,
}

#[derive(Args)]
struct BundleArgs {
    #[arg(short, long, allow_hyphen_values = true)]
    prompt: Option<String>,

    #[arg(long)]
    prompt_file: Option<PathBuf>,

    #[arg(long, short = 'f')]
    files: Vec<String>,

    #[arg(long)]
    exclude: Vec<String>,

    #[arg(long, default_value = "200000")]
    max_file_bytes: usize,

    #[arg(long, default_value = "5000000")]
    max_total_bytes: usize,

    #[arg(long)]
    all: bool,
}

#[derive(Args)]
struct SessionArgs {
    id: String,
}

#[derive(Args)]
struct BrowserArgs {
    #[command(subcommand)]
    command: BrowserCommand,
}

#[derive(Subcommand)]
enum BrowserCommand {
    Exec(BrowserExecArgs),
    Recipe(BrowserRecipeArgs),
    Login(BrowserLoginArgs),
    Check(BrowserCheckArgs),
    /// Sync cookies from Chrome to agent-browser (bypasses Cloudflare)
    SyncCookies(BrowserSyncCookiesArgs),
    /// Attach to a running Chrome instance and verify authentication.
    /// Auto-discovers via chrome://inspect (Chrome 144+), or use --cdp for explicit endpoint.
    Attach(BrowserAttachArgs),
}

#[derive(Args)]
struct BrowserExecArgs {
    #[arg(trailing_var_arg = true)]
    args: Vec<String>,
}

#[derive(Args)]
struct BrowserRecipeArgs {
    #[arg(long)]
    recipe: PathBuf,

    #[arg(long)]
    bundle: Option<PathBuf>,

    #[arg(long)]
    profile: Option<PathBuf>,

    /// Connect to Chrome via CDP endpoint (e.g. http://127.0.0.1:9222)
    #[arg(long)]
    cdp: Option<String>,

    #[arg(long = "var", value_name = "KEY=VALUE")]
    vars: Vec<String>,
}

#[derive(Args)]
struct BrowserLoginArgs {
    #[arg(long)]
    profile: Option<PathBuf>,
    /// Connect to Chrome via CDP (explicit only, no auto-discovery for login)
    #[arg(long)]
    cdp: Option<String>,
}

#[derive(Args)]
struct BrowserCheckArgs {
    #[arg(long)]
    profile: Option<PathBuf>,
    /// CDP endpoint (e.g. http://127.0.0.1:9222). Falls back to YOETZ_BROWSER_CDP env
    /// or config, then chrome://inspect auto-connect (Chrome 144+).
    #[arg(long)]
    cdp: Option<String>,
}

#[derive(Args)]
struct BrowserAttachArgs {
    /// CDP endpoint (e.g. http://127.0.0.1:9222). Falls back to YOETZ_BROWSER_CDP env
    /// or config, then chrome://inspect auto-connect (Chrome 144+).
    #[arg(long)]
    cdp: Option<String>,
}

#[derive(Args)]
struct BrowserSyncCookiesArgs {
    #[arg(long)]
    profile: Option<PathBuf>,
}

#[derive(Args)]
struct CouncilArgs {
    #[arg(short, long, allow_hyphen_values = true)]
    prompt: Option<String>,

    #[arg(long)]
    prompt_file: Option<PathBuf>,

    #[arg(long, short = 'f')]
    files: Vec<String>,

    #[arg(long)]
    exclude: Vec<String>,

    #[arg(long, default_value = "200000")]
    max_file_bytes: usize,

    #[arg(long, default_value = "5000000")]
    max_total_bytes: usize,

    #[arg(long, value_delimiter = ',')]
    models: Vec<String>,

    #[arg(long)]
    provider: Option<String>,

    #[arg(long, default_value = "0.1")]
    temperature: f32,

    #[arg(long)]
    max_output_tokens: Option<usize>,

    #[arg(long, default_value = "4")]
    max_parallel: usize,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    max_cost_usd: Option<f64>,

    #[arg(long)]
    daily_budget_usd: Option<f64>,

    #[arg(long, value_name = "json|text")]
    response_format: Option<String>,

    #[arg(long)]
    response_schema: Option<PathBuf>,

    #[arg(long)]
    response_schema_name: Option<String>,
}

#[derive(Args)]
struct ApplyArgs {
    #[arg(long)]
    patch_file: Option<PathBuf>,

    #[arg(long)]
    check: bool,

    #[arg(long)]
    reverse: bool,
}

#[derive(Args)]
struct ReviewArgs {
    #[command(subcommand)]
    command: ReviewCommand,
}

#[derive(Subcommand)]
enum ReviewCommand {
    Diff(ReviewDiffArgs),
    File(ReviewFileArgs),
}

#[derive(Args)]
struct GenerateArgs {
    #[command(subcommand)]
    command: GenerateCommand,
}

#[derive(Subcommand)]
enum GenerateCommand {
    Image(GenerateImageArgs),
    Video(GenerateVideoArgs),
}

#[derive(Args)]
struct GenerateImageArgs {
    #[arg(short, long, allow_hyphen_values = true)]
    prompt: Option<String>,

    #[arg(long)]
    prompt_file: Option<PathBuf>,

    #[arg(long)]
    provider: Option<String>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long, value_name = "PATH_OR_URL")]
    image: Vec<String>,

    #[arg(
        long,
        value_name = "MIME",
        help = "Override MIME type for --image inputs (1 value or 1 per image)"
    )]
    image_mime: Vec<String>,

    #[arg(long)]
    size: Option<String>,

    #[arg(long)]
    quality: Option<String>,

    #[arg(long)]
    background: Option<String>,

    #[arg(long, default_value = "1")]
    n: usize,

    #[arg(long)]
    output_dir: Option<PathBuf>,

    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct GenerateVideoArgs {
    #[arg(short, long, allow_hyphen_values = true)]
    prompt: Option<String>,

    #[arg(long)]
    prompt_file: Option<PathBuf>,

    #[arg(long)]
    provider: Option<String>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long, value_name = "PATH_OR_URL")]
    image: Vec<String>,

    #[arg(
        long,
        value_name = "MIME",
        help = "Override MIME type for --image inputs (1 value or 1 per image)"
    )]
    image_mime: Vec<String>,

    #[arg(long)]
    duration_secs: Option<u32>,

    #[arg(long)]
    aspect_ratio: Option<String>,

    #[arg(long)]
    resolution: Option<String>,

    #[arg(long)]
    size: Option<String>,

    #[arg(long)]
    negative_prompt: Option<String>,

    #[arg(long)]
    output_dir: Option<PathBuf>,

    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct ReviewDiffArgs {
    #[arg(long, allow_hyphen_values = true)]
    prompt: Option<String>,

    #[arg(long)]
    staged: bool,

    #[arg(long)]
    paths: Vec<String>,

    #[arg(long)]
    provider: Option<String>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long, default_value = "0.1")]
    temperature: f32,

    #[arg(long)]
    max_output_tokens: Option<usize>,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    max_cost_usd: Option<f64>,

    #[arg(long)]
    daily_budget_usd: Option<f64>,

    #[arg(long, value_name = "json|text")]
    response_format: Option<String>,

    #[arg(long)]
    response_schema: Option<PathBuf>,

    #[arg(long)]
    response_schema_name: Option<String>,

    /// Maximum diff size in bytes before truncation (default: 500000)
    #[arg(long, default_value = "500000")]
    max_diff_bytes: usize,
}

#[derive(Args)]
struct ReviewFileArgs {
    #[arg(long)]
    path: PathBuf,

    #[arg(long, allow_hyphen_values = true)]
    prompt: Option<String>,

    #[arg(long)]
    provider: Option<String>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long, default_value = "0.1")]
    temperature: f32,

    #[arg(long)]
    max_output_tokens: Option<usize>,

    #[arg(long)]
    max_file_bytes: Option<usize>,

    #[arg(long)]
    max_total_bytes: Option<usize>,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    max_cost_usd: Option<f64>,

    #[arg(long)]
    daily_budget_usd: Option<f64>,

    #[arg(long, value_name = "json|text")]
    response_format: Option<String>,

    #[arg(long)]
    response_schema: Option<PathBuf>,

    #[arg(long)]
    response_schema_name: Option<String>,
}

#[derive(Args)]
struct ModelsArgs {
    #[command(subcommand)]
    command: ModelsCommand,
}

#[derive(Subcommand)]
enum ModelsCommand {
    List(ModelsListArgs),
    Sync,
    /// Fuzzy-resolve a model ID query against the registry
    Resolve(ModelsResolveArgs),
    /// Show the frontier model per major provider family
    Frontier(ModelsFrontierArgs),
}

#[derive(Args)]
struct ModelsListArgs {
    /// Fuzzy-search models by ID (ranked by relevance)
    #[arg(long, short = 's')]
    search: Option<String>,

    /// Filter by provider name
    #[arg(long)]
    provider: Option<String>,
}

#[derive(Args)]
struct ModelsResolveArgs {
    /// The model ID query to resolve (e.g. "grok-4.1", "claude-sonnet")
    query: String,

    /// Maximum number of results to return
    #[arg(long, short = 'n', default_value = "5")]
    max_results: usize,
}

#[derive(Args)]
struct ModelsFrontierArgs {
    /// Filter to a specific provider family (e.g. "openai", "anthropic")
    #[arg(long)]
    family: Option<String>,

    /// Show all provider families (default: major frontier labs only)
    #[arg(long)]
    all: bool,
}

#[derive(Args)]
struct PricingArgs {
    #[command(subcommand)]
    command: PricingCommand,
}

#[derive(Subcommand)]
enum PricingCommand {
    Estimate(PricingEstimateArgs),
}

#[derive(Args)]
struct PricingEstimateArgs {
    #[arg(long)]
    model: String,

    #[arg(long)]
    input_tokens: usize,

    #[arg(long)]
    output_tokens: usize,
}

struct CallResult {
    content: String,
    usage: Usage,
    response_id: Option<String>,
    header_cost: Option<f64>,
}

#[derive(Debug, Serialize)]
struct ReviewResult {
    id: String,
    provider: String,
    model: String,
    pricing: PricingEstimate,
    usage: Usage,
    content: String,
    artifacts: ArtifactPaths,
}

#[derive(Debug, Serialize)]
struct CouncilResult {
    id: String,
    provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    bundle: Option<yoetz_core::types::Bundle>,
    results: Vec<CouncilModelResult>,
    pricing: CouncilPricing,
    usage: Usage,
    artifacts: ArtifactPaths,
}

#[derive(Debug, Serialize)]
struct CouncilModelResult {
    model: String,
    content: String,
    usage: Usage,
    pricing: PricingEstimate,
    response_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct CouncilPricing {
    estimate_usd_total: Option<f64>,
    per_model: Vec<ModelEstimate>,
}

#[derive(Debug, Serialize)]
struct ModelEstimate {
    model: String,
    estimate_usd: Option<f64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Capture security-sensitive env vars before dotenv loading.
    // CWD .env files must not override executable paths (supply-chain risk)
    // or redirect API keys to attacker-controlled endpoints.
    let pre_agent_bin = env::var("YOETZ_AGENT_BROWSER_BIN").ok();
    let pre_scripts_dir = env::var("YOETZ_SCRIPTS_DIR").ok();

    // Capture API key env vars so CWD .env cannot silently replace them.
    const PROTECTED_API_KEYS: &[&str] = &[
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "GEMINI_API_KEY",
        "OPENROUTER_API_KEY",
        "XAI_API_KEY",
    ];
    let pre_api_keys: Vec<(&str, Option<String>)> = PROTECTED_API_KEYS
        .iter()
        .map(|&k| (k, env::var(k).ok()))
        .collect();

    // Load environment files (.env.local takes precedence over .env)
    dotenvy::from_filename(".env.local").ok();
    dotenvy::dotenv().ok();

    // Prevent CWD .env from overriding executable paths (security)
    if pre_agent_bin.is_none() {
        env::remove_var("YOETZ_AGENT_BROWSER_BIN");
    }
    if pre_scripts_dir.is_none() {
        env::remove_var("YOETZ_SCRIPTS_DIR");
    }

    // Restore API key env vars if .env changed them (prevent credential hijack)
    for (key, pre_value) in &pre_api_keys {
        let post_value = env::var(key).ok();
        if post_value != *pre_value {
            match pre_value {
                Some(v) => env::set_var(key, v),
                None => env::remove_var(key),
            }
            eprintln!("warning: CWD .env tried to override {key}, ignored");
        }
    }

    let cli = Cli::parse();
    let format = resolve_format(cli.format.as_deref())?;

    if cli.debug {
        env::set_var("YOETZ_GEMINI_DEBUG", "1");
        env::set_var("LITELLM_GEMINI_DEBUG", "1");
    }
    let config = Config::load_with_profile(cli.profile.as_deref())?;
    let client = build_client(cli.timeout_secs)?;
    let litellm = std::sync::Arc::new(build_litellm(&config, client.clone())?);
    let ctx = AppContext {
        config,
        client,
        litellm,
        output_final: cli.output_final,
        output_schema: cli.output_schema,
        debug: cli.debug,
        allow_unknown: cli.allow_unknown,
    };

    match cli.command {
        Commands::Ask(args) => commands::ask::handle_ask(&ctx, args, format).await,
        Commands::Bundle(args) => commands::bundle::handle_bundle(&ctx, args, format),
        Commands::Status => handle_status(&ctx, format),
        Commands::Session(args) => handle_session(&ctx, args, format),
        Commands::Models(args) => commands::models::handle_models(&ctx, args, format).await,
        Commands::Pricing(args) => commands::pricing::handle_pricing(&ctx, args, format).await,
        Commands::Browser(args) => handle_browser(&ctx, args, format),
        Commands::Council(args) => commands::council::handle_council(&ctx, args, format).await,
        Commands::Apply(args) => commands::apply::handle_apply(args),
        Commands::Review(args) => commands::review::handle_review(&ctx, args, format).await,
        Commands::Generate(args) => commands::generate::handle_generate(&ctx, args, format).await,
    }
}

fn resolve_format(flag: Option<&str>) -> Result<OutputFormat> {
    if let Some(fmt) = flag {
        return fmt.parse();
    }
    if env::var("YOETZ_AGENT").ok().as_deref() == Some("1") {
        return Ok(OutputFormat::Json);
    }
    Ok(OutputFormat::Text)
}

fn build_client(timeout_secs: u64) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(10))
        .build()?)
}

fn build_litellm(config: &Config, client: reqwest::Client) -> Result<LiteLLM> {
    let mut litellm = LiteLLM::new()?.with_client(client);
    if let Some(default_provider) = config.defaults.provider.as_deref() {
        litellm = litellm.with_default_provider(default_provider);
    }
    for (name, provider) in &config.providers {
        let mut cfg = LiteProviderConfig::default();
        if let Some(base) = &provider.base_url {
            cfg = cfg.with_base_url(base.clone());
        }
        if let Some(env) = &provider.api_key_env {
            cfg = cfg.with_api_key_env(env.clone());
        }
        let kind = map_provider_kind(provider.kind.as_deref(), name);
        cfg = cfg.with_kind(kind);
        litellm = litellm.with_provider(name.clone(), cfg);
    }
    Ok(litellm)
}

fn map_provider_kind(kind: Option<&str>, name: &str) -> LiteProviderKind {
    let key = kind
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| name.to_lowercase());
    match key.as_str() {
        "anthropic" => LiteProviderKind::Anthropic,
        "gemini" => LiteProviderKind::Gemini,
        "openai" | "openai_compatible" | "openai-compatible" | "openai-compat" | "openrouter"
        | "xai" | "litellm" => LiteProviderKind::OpenAICompatible,
        _ => {
            if name.eq_ignore_ascii_case("anthropic") {
                LiteProviderKind::Anthropic
            } else if name.eq_ignore_ascii_case("gemini") {
                LiteProviderKind::Gemini
            } else {
                LiteProviderKind::OpenAICompatible
            }
        }
    }
}

fn handle_status(ctx: &AppContext, format: OutputFormat) -> Result<()> {
    let sessions = list_sessions()?;
    maybe_write_output(ctx, &sessions)?;
    match format {
        OutputFormat::Json => write_json(&sessions),
        OutputFormat::Jsonl => write_jsonl("status", &sessions),
        OutputFormat::Text | OutputFormat::Markdown => {
            for s in sessions {
                println!("{}\t{}", s.id, s.path.display());
            }
            Ok(())
        }
    }
}

fn handle_session(ctx: &AppContext, args: SessionArgs, format: OutputFormat) -> Result<()> {
    let base = yoetz_core::session::session_base_dir();
    let path = base.join(&args.id);
    if !path.exists() {
        return Err(anyhow!("session not found: {}", args.id));
    }
    maybe_write_output(ctx, &path)?;
    match format {
        OutputFormat::Json => write_json(&path),
        OutputFormat::Jsonl => write_jsonl("session", &path),
        OutputFormat::Text | OutputFormat::Markdown => {
            println!("{}", path.display());
            Ok(())
        }
    }
}

fn handle_browser(ctx: &AppContext, args: BrowserArgs, format: OutputFormat) -> Result<()> {
    match args.command {
        BrowserCommand::Exec(exec) => {
            let stdout = browser::run_agent_browser(exec.args, format, None)?;
            print!("{stdout}");
            Ok(())
        }
        BrowserCommand::Login(login_args) => {
            let profile_dir =
                browser::resolve_profile_dir(&ctx.config, login_args.profile.as_ref())?;

            // If --cdp explicitly passed, try CDP first (login is conservative:
            // no auto-discovery unless user explicitly requests it)
            if let Some(ref cdp_url) = login_args.cdp {
                if browser::try_cdp_attach(cdp_url, "https://chatgpt.com/").is_ok() {
                    let payload = json!({
                        "status": "ok",
                        "method": "cdp_explicit",
                        "endpoint": cdp_url,
                        "profile": profile_dir.to_string_lossy(),
                    });
                    return match format {
                        OutputFormat::Json => write_json(&payload),
                        OutputFormat::Jsonl => write_jsonl("browser.login", &payload),
                        OutputFormat::Text | OutputFormat::Markdown => {
                            println!("Authenticated via CDP: {cdp_url}");
                            Ok(())
                        }
                    };
                }
                eprintln!("CDP attach to {cdp_url} failed, falling back to cookie sync.");
            }

            let mut used_cookie_sync = false;
            let mut cookie_warnings = Vec::new();
            let mut cookie_sync_error: Option<String> = None;
            if matches!(format, OutputFormat::Text | OutputFormat::Markdown) {
                if let Some(guidance) = browser::cookie_sync_guidance() {
                    eprintln!("{guidance}");
                }
            }
            match browser::sync_cookies(&profile_dir) {
                Ok((count, warnings)) => {
                    used_cookie_sync = true;
                    cookie_warnings = warnings;
                    if browser::check_auth(&profile_dir, /* headed */ false).is_ok() {
                        let payload = json!({
                            "status": "ok",
                            "profile": profile_dir.to_string_lossy(),
                            "cookies_synced": true,
                            "cookie_count": count,
                            "warnings": cookie_warnings,
                            "next": "Run `yoetz browser check` to verify authentication."
                        });
                        return match format {
                            OutputFormat::Json => write_json(&payload),
                            OutputFormat::Jsonl => write_jsonl("browser.login", &payload),
                            OutputFormat::Text | OutputFormat::Markdown => {
                                println!("Cookies synced from Chrome ({} cookies).", count);
                                println!("Profile: {}", profile_dir.display());
                                if !cookie_warnings.is_empty() {
                                    eprintln!("Warnings: {}", cookie_warnings.join("; "));
                                }
                                println!("Next: yoetz browser check");
                                Ok(())
                            }
                        };
                    }
                }
                Err(err) => {
                    cookie_sync_error = Some(err.to_string());
                }
            }

            if used_cookie_sync {
                eprintln!(
                    "Cookie sync succeeded but auth check failed. Falling back to manual login."
                );
            } else if let Some(err) = cookie_sync_error.as_ref() {
                eprintln!("Cookie sync failed: {err}");
                eprintln!("Falling back to manual login.");
            }
            browser::login(&profile_dir)?;
            let payload = json!({
                "status": "pending_login",
                "profile": profile_dir.to_string_lossy(),
                "cookies_synced": used_cookie_sync,
                "warnings": cookie_warnings,
                "cookie_sync_error": cookie_sync_error,
                "next": "Complete login in the opened browser, then run `yoetz browser check` to verify authentication."
            });
            match format {
                OutputFormat::Json => write_json(&payload),
                OutputFormat::Jsonl => write_jsonl("browser.login", &payload),
                OutputFormat::Text | OutputFormat::Markdown => {
                    println!("Browser opened for manual login: {}", profile_dir.display());
                    println!("Complete login in the opened browser, then run: yoetz browser check");
                    Ok(())
                }
            }
        }
        BrowserCommand::Check(check_args) => {
            let profile_dir =
                browser::resolve_profile_dir(&ctx.config, check_args.profile.as_ref())?;
            let connection = browser::resolve_browser_connection(
                &ctx.config,
                check_args.cdp.as_deref(),
                &profile_dir,
                "https://chatgpt.com/",
            )?;
            let method = match &connection {
                browser::BrowserConnection::Cdp { endpoint } => format!("cdp: {endpoint}"),
                browser::BrowserConnection::AutoConnect => "auto_connect".to_string(),
                browser::BrowserConnection::CookieState { .. } => "cookie_state".to_string(),
                browser::BrowserConnection::Profile { .. } => "profile".to_string(),
            };
            let payload = json!({
                "status": "ok",
                "profile": profile_dir.to_string_lossy(),
                "method": method,
            });
            match format {
                OutputFormat::Json => write_json(&payload),
                OutputFormat::Jsonl => write_jsonl("browser.check", &payload),
                OutputFormat::Text | OutputFormat::Markdown => {
                    println!("Browser authenticated via {method}");
                    Ok(())
                }
            }
        }
        BrowserCommand::SyncCookies(sync_args) => {
            let profile_dir =
                browser::resolve_profile_dir(&ctx.config, sync_args.profile.as_ref())?;
            if matches!(format, OutputFormat::Text | OutputFormat::Markdown) {
                if let Some(guidance) = browser::cookie_sync_guidance() {
                    eprintln!("{guidance}");
                }
            }
            let (cookie_count, warnings) = browser::sync_cookies(&profile_dir)?;
            let state_file = browser::state_file(&profile_dir);
            let payload = json!({
                "status": "ok",
                "profile": profile_dir.to_string_lossy(),
                "state_file": state_file.to_string_lossy(),
                "cookie_count": cookie_count,
                "warnings": warnings,
            });
            match format {
                OutputFormat::Json => write_json(&payload),
                OutputFormat::Jsonl => write_jsonl("browser.sync_cookies", &payload),
                OutputFormat::Text | OutputFormat::Markdown => {
                    println!(
                        "Cookies synced ({} cookies) to: {}",
                        cookie_count,
                        state_file.display()
                    );
                    if !warnings.is_empty() {
                        eprintln!("Warnings: {}", warnings.join("; "));
                    }
                    println!("Next: yoetz browser check");
                    Ok(())
                }
            }
        }
        BrowserCommand::Recipe(recipe_args) => {
            let recipe_path = browser::resolve_recipe(&recipe_args.recipe)
                .with_context(|| format!("resolve recipe {:?}", recipe_args.recipe))?;
            let content = fs::read_to_string(&recipe_path)
                .with_context(|| format!("read recipe {}", recipe_path.display()))?;
            let recipe: browser::Recipe = serde_yaml::from_str(&content)?;
            let recipe_vars =
                browser::build_recipe_vars(recipe.defaults.as_ref(), &recipe_args.vars)?;
            let profile_dir =
                browser::resolve_profile_dir(&ctx.config, recipe_args.profile.as_ref())?;
            let recipe_name = recipe.name.as_deref().unwrap_or("");
            let needs_auth = recipe_name.eq_ignore_ascii_case("chatgpt")
                || recipe_path
                    .to_string_lossy()
                    .to_lowercase()
                    .contains("chatgpt");
            // Try CDP or auto-connect for a live browser connection. Falls back to
            // cookie/profile if neither is available. Uses the lite auto-connect check
            // for recipes — verifies Chrome is reachable without opening new tabs.
            let live_connection = if needs_auth {
                if let Some(endpoint) =
                    browser::resolve_cdp_endpoint(recipe_args.cdp.as_deref(), &ctx.config)
                {
                    match browser::try_cdp_attach(&endpoint, browser::CHATGPT_URL) {
                        Ok(()) => Some(browser::BrowserConnection::Cdp { endpoint }),
                        Err(e) => {
                            if recipe_args.cdp.is_some() {
                                return Err(e.context("explicit --cdp failed; not falling back"));
                            }
                            None
                        }
                    }
                } else if browser::try_auto_connect_lite().is_ok() {
                    Some(browser::BrowserConnection::AutoConnect)
                } else {
                    None
                }
            } else {
                None
            };
            let profile_mode = if live_connection.is_some() {
                browser::BrowserProfileMode::ProfileOnly
            } else if needs_auth {
                browser::resolve_auth_mode(&profile_dir, /* headed */ false)?
            } else {
                browser::BrowserProfileMode::ProfileOnly
            };

            // Only read bundle text if the recipe actually references it. Some recipes
            // (e.g. ChatGPT file-upload flows) only need {{bundle_path}}.
            // Check for both {{bundle_text}} and {{bundle_text|json}}.
            let needs_bundle_text = recipe.steps.iter().any(|step| {
                step.args
                    .as_ref()
                    .map(|args| {
                        args.iter()
                            .any(|a| a.contains("{{bundle_text}}") || a.contains("{{bundle_text|json}}"))
                    })
                    .unwrap_or(false)
            });
            let bundle_text = match (needs_bundle_text, recipe_args.bundle.as_ref()) {
                (true, Some(path)) => Some(fs::read_to_string(path)?),
                _ => None,
            };

            let ctx = browser::RecipeContext {
                bundle_path: recipe_args.bundle.map(|p| p.to_string_lossy().to_string()),
                bundle_text,
                profile_dir: Some(profile_dir),
                profile_mode,
                use_stealth: needs_auth,
                headed: needs_auth,
                vars: recipe_vars,
            };

            if let Some(connection) = live_connection {
                browser::run_recipe_with_live_connection(recipe, ctx, &connection, format)
            } else {
                browser::run_recipe(recipe, ctx, format)
            }
        }
        BrowserCommand::Attach(attach_args) => {
            // Try explicit CDP first, then auto-connect. No cookie fallback for attach.
            let cdp_endpoint =
                browser::resolve_cdp_endpoint(attach_args.cdp.as_deref(), &ctx.config);

            if let Some(ref endpoint) = cdp_endpoint {
                if browser::try_cdp_attach(endpoint, "https://chatgpt.com/").is_ok() {
                    let payload = json!({
                        "status": "ok",
                        "method": "cdp_explicit",
                        "endpoint": endpoint,
                    });
                    return match format {
                        OutputFormat::Json => write_json(&payload),
                        OutputFormat::Jsonl => write_jsonl("browser.attach", &payload),
                        OutputFormat::Text | OutputFormat::Markdown => {
                            println!("Attached via CDP: {endpoint}");
                            Ok(())
                        }
                    };
                }
            }

            if browser::try_auto_connect("https://chatgpt.com/").is_ok() {
                let payload = json!({
                    "status": "ok",
                    "method": "auto_connect",
                });
                return match format {
                    OutputFormat::Json => write_json(&payload),
                    OutputFormat::Jsonl => write_jsonl("browser.attach", &payload),
                    OutputFormat::Text | OutputFormat::Markdown => {
                        println!("Attached via Chrome auto-connect");
                        Ok(())
                    }
                };
            }

            Err(anyhow!(
                "could not attach to any Chrome instance.\n\n\
                 Recommended: enable remote debugging at chrome://inspect/#remote-debugging (Chrome 144+)\n\
                 Alternative: pass --cdp <url> with Chrome launched using --user-data-dir\n\n\
                 Note: since Chrome 136, --remote-debugging-port is ignored on the default profile.\n\
                 See: https://developer.chrome.com/blog/remote-debugging-port"
            ))
        }
    }
}

fn build_review_diff_prompt(diff: &str, extra_prompt: Option<&str>) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are a senior engineer performing a careful code review. ");
    prompt.push_str("Return JSON only with fields: summary, findings[], risks, patches.\n");
    prompt.push_str("Each finding: {severity, file, line, message, suggestion}.\n");
    prompt.push_str("Include a unified diff in patches if needed.\n");
    if let Some(extra) = extra_prompt {
        prompt.push_str("\nAdditional instructions:\n");
        prompt.push_str(extra);
        prompt.push('\n');
    }
    prompt.push_str("\nDiff:\n```diff\n");
    prompt.push_str(diff);
    prompt.push_str("\n```\n");
    prompt
}

fn build_review_file_prompt(
    path: &std::path::Path,
    content: &str,
    truncated: bool,
    extra_prompt: Option<&str>,
) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are a senior engineer reviewing a single file. ");
    prompt.push_str("Return JSON only with fields: summary, findings[], risks, patches.\n");
    prompt.push_str("Each finding: {severity, file, line, message, suggestion}.\n");
    prompt.push_str("Include a unified diff in patches if needed.\n");
    if let Some(extra) = extra_prompt {
        prompt.push_str("\nAdditional instructions:\n");
        prompt.push_str(extra);
        prompt.push('\n');
    }
    prompt.push_str(&format!("\nFile: {}\n", path.display()));
    prompt.push_str("```text\n");
    prompt.push_str(content);
    if truncated {
        prompt.push_str("\n... [truncated]\n");
    }
    prompt.push_str("```\n");
    prompt
}

fn git_diff(staged: bool, paths: &[String]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("diff");
    cmd.arg("--no-color");
    if staged {
        cmd.arg("--staged");
    }
    if !paths.is_empty() {
        cmd.arg("--");
        for p in paths {
            cmd.arg(p);
        }
    }
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(anyhow!("git diff failed"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn read_text_file(path: &std::path::Path, max_bytes: usize) -> Result<(String, bool)> {
    let metadata = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    let truncated = metadata.len() as usize > max_bytes;
    let mut file = fs::File::open(path).with_context(|| format!("read {}", path.display()))?;
    let mut data = vec![0u8; max_bytes];
    let read = file.read(&mut data)?;
    data.truncate(read);
    let slice = if truncated {
        &data[..max_bytes.min(data.len())]
    } else {
        &data
    };
    if slice.contains(&0) {
        return Err(anyhow!("file appears to be binary"));
    }
    match std::str::from_utf8(slice) {
        Ok(text) => Ok((text.to_string(), truncated)),
        Err(e) if truncated && e.valid_up_to() > 0 => {
            let valid = e.valid_up_to();
            let text = std::str::from_utf8(&slice[..valid]).unwrap_or("");
            Ok((text.to_string(), true))
        }
        Err(_) => Err(anyhow!("file is not valid UTF-8")),
    }
}

/// Add usage statistics together.
fn add_usage(mut total: Usage, usage: &Usage) -> Usage {
    total.add(usage);
    total
}

fn maybe_write_output<T: Serialize>(ctx: &AppContext, value: &T) -> Result<()> {
    if ctx.output_final.is_none() && ctx.output_schema.is_none() {
        return Ok(());
    }
    let json = serde_json::to_value(value)?;
    if let Some(schema_path) = ctx.output_schema.as_ref() {
        validate_output_schema(schema_path, &json)?;
    }
    if let Some(path) = ctx.output_final.as_ref() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        write_json_file(path, &json)?;
    }
    Ok(())
}

fn validate_output_schema(path: &std::path::Path, value: &Value) -> Result<()> {
    let schema_text =
        fs::read_to_string(path).with_context(|| format!("read schema {}", path.display()))?;
    let schema_json: Value = serde_json::from_str(&schema_text)?;
    let compiled = Validator::new(&schema_json)
        .map_err(|e| anyhow!("invalid schema {}: {e}", path.display()))?;
    let result = compiled.validate(value);
    if let Err(err) = result {
        return Err(anyhow!(
            "output does not match schema {}: {}",
            path.display(),
            err
        ));
    }
    Ok(())
}

fn resolve_prompt(prompt: Option<String>, prompt_file: Option<PathBuf>) -> Result<String> {
    if let Some(p) = prompt {
        return Ok(p);
    }
    if let Some(path) = prompt_file {
        let content = fs::read_to_string(path)?;
        return Ok(content);
    }
    let mut buf = String::new();
    if !io::stdin().is_terminal() {
        io::stdin().read_to_string(&mut buf)?;
        if !buf.trim().is_empty() {
            return Ok(buf);
        }
    }
    Err(anyhow!(
        "prompt is required (--prompt, --prompt-file, or stdin)"
    ))
}

fn resolve_response_format(
    format: Option<String>,
    schema_path: Option<PathBuf>,
    schema_name: Option<String>,
) -> Result<Option<Value>> {
    if let Some(path) = schema_path {
        let schema_text =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let schema_json: Value = serde_json::from_str(&schema_text)
            .with_context(|| format!("parse schema {}", path.display()))?;
        if !schema_json.is_object() {
            return Err(anyhow!(
                "response schema must be a JSON object: {}",
                path.display()
            ));
        }
        let name = schema_name.unwrap_or_else(|| "yoetz_response".to_string());
        if let Some(fmt) = format.as_deref() {
            if fmt.eq_ignore_ascii_case("text") {
                return Err(anyhow!(
                    "--response_format=text is incompatible with --response_schema"
                ));
            }
        }
        return Ok(Some(serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": name,
                "schema": schema_json,
                "strict": true,
            }
        })));
    }

    let format = match format.as_deref() {
        Some("json") | Some("json_object") => Some(serde_json::json!({ "type": "json_object" })),
        Some("text") | None => None,
        Some(other) => {
            return Err(anyhow!(
                "unsupported response_format: {other} (use json or text)"
            ))
        }
    };
    Ok(format)
}

fn parse_media_inputs(
    values: &[String],
    mimes: &[String],
    kind: MediaType,
) -> Result<Vec<MediaInput>> {
    let kind_label = media_type_label(&kind);
    let overrides = normalize_mime_overrides(values.len(), mimes, kind_label)?;
    let mut out = Vec::with_capacity(values.len());
    for (value, mime) in values.iter().zip(overrides.into_iter()) {
        out.push(parse_media_input(value, mime.as_deref(), kind.clone())?);
    }
    Ok(out)
}

fn normalize_mime_overrides(
    values_len: usize,
    mimes: &[String],
    kind: &str,
) -> Result<Vec<Option<String>>> {
    if mimes.is_empty() {
        return Ok(vec![None; values_len]);
    }
    if values_len == 0 {
        return Err(anyhow!("{kind} mime provided but no {kind} inputs"));
    }
    if mimes.len() == 1 && values_len > 1 {
        return Ok(vec![Some(mimes[0].clone()); values_len]);
    }
    if mimes.len() == values_len {
        return Ok(mimes.iter().cloned().map(Some).collect());
    }
    Err(anyhow!(
        "expected 1 or {values_len} {kind} mime values, got {}",
        mimes.len()
    ))
}

fn parse_media_input(value: &str, mime: Option<&str>, kind: MediaType) -> Result<MediaInput> {
    if value.starts_with("http://") || value.starts_with("https://") || value.starts_with("gs://") {
        return MediaInput::from_url_with_type(value, kind, mime);
    }
    let input = MediaInput::from_path_with_mime(PathBuf::from(value).as_path(), mime)?;
    if input.media_type != kind {
        return Err(anyhow!(
            "expected {label} input but got {mime} (use a mime override to force it)",
            label = media_type_label(&kind),
            mime = input.mime_type
        ));
    }
    Ok(input)
}

fn media_type_label(kind: &MediaType) -> &'static str {
    match kind {
        MediaType::Image => "image",
        MediaType::Video => "video",
    }
}

fn render_bundle_md(bundle: &yoetz_core::types::Bundle) -> String {
    let mut out = String::new();
    out.push_str("# Yoetz Bundle\n\n");
    out.push_str("## Prompt\n\n");
    out.push_str(&bundle.prompt);
    out.push_str("\n\n## Files\n\n");
    for file in &bundle.files {
        out.push_str(&format!("### {}\n\n", file.path));
        if let Some(content) = &file.content {
            let fence = markdown_fence(content);
            out.push_str(&fence);
            out.push('\n');
            out.push_str(content);
            if !content.ends_with('\n') {
                out.push('\n');
            }
            if file.truncated {
                out.push_str("\n... [truncated]\n");
            }
            out.push_str(&fence);
            out.push_str("\n\n");
        } else if file.is_binary {
            out.push_str("(binary file omitted)\n\n");
        } else if file.truncated {
            out.push_str("(content omitted)\n\n");
        }
    }
    out
}

fn markdown_fence(content: &str) -> String {
    let mut max_run = 0usize;
    let mut current = 0usize;
    for ch in content.chars() {
        if ch == '`' {
            current += 1;
            if current > max_run {
                max_run = current;
            }
        } else {
            current = 0;
        }
    }
    let len = std::cmp::max(3, max_run + 1);
    "`".repeat(len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn normalize_model_name(model: &str) -> String {
        normalize_model_name_with_aliases(model, &std::collections::HashMap::new())
    }

    fn temp_schema_path() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("yoetz_schema_{nanos}.json"))
    }

    #[test]
    fn response_format_json_object() {
        let fmt = resolve_response_format(Some("json".to_string()), None, None).unwrap();
        assert!(fmt.is_some());
    }

    #[test]
    fn response_format_schema_file() {
        let path = temp_schema_path();
        fs::write(
            &path,
            r#"{"type":"object","properties":{"ok":{"type":"boolean"}}}"#,
        )
        .unwrap();
        let fmt = resolve_response_format(None, Some(path.clone()), None).unwrap();
        assert!(fmt.is_some());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn normalize_model_name_bare_aliases() {
        assert_eq!(normalize_model_name("gemini-pro-3"), "gemini-3-pro-preview");
        assert_eq!(
            normalize_model_name("gemini-flash-3"),
            "gemini-3-flash-preview"
        );
    }

    #[test]
    fn normalize_model_name_prefixed_aliases() {
        assert_eq!(
            normalize_model_name("gemini/gemini-pro-3"),
            "gemini/gemini-3-pro-preview"
        );
        assert_eq!(
            normalize_model_name("gemini/gemini-flash-3"),
            "gemini/gemini-3-flash-preview"
        );
        assert_eq!(
            normalize_model_name("openrouter/google/gemini-pro-3"),
            "openrouter/google/gemini-3-pro-preview"
        );
        assert_eq!(
            normalize_model_name("openrouter/google/gemini-flash-3"),
            "openrouter/google/gemini-3-flash-preview"
        );
    }

    #[test]
    fn normalize_model_name_case_insensitive() {
        assert_eq!(normalize_model_name("Gemini-Pro-3"), "gemini-3-pro-preview");
        assert_eq!(
            normalize_model_name("GEMINI/GEMINI-FLASH-3"),
            "gemini/gemini-3-flash-preview"
        );
    }

    #[test]
    fn normalize_model_name_google_prefix() {
        assert_eq!(
            normalize_model_name("google/gemini-pro-3"),
            "google/gemini-3-pro-preview"
        );
    }

    #[test]
    fn normalize_model_name_with_suffix() {
        assert_eq!(
            normalize_model_name("openrouter/google/gemini-pro-3:free"),
            "openrouter/google/gemini-3-pro-preview:free"
        );
        assert_eq!(
            normalize_model_name("gemini-flash-3:extended"),
            "gemini-3-flash-preview:extended"
        );
    }

    #[test]
    fn normalize_model_name_passthrough() {
        assert_eq!(
            normalize_model_name("gemini-3-pro-preview"),
            "gemini-3-pro-preview"
        );
        assert_eq!(normalize_model_name("gpt-5.2"), "gpt-5.2");
        // Preserve suffix on non-matching models
        assert_eq!(normalize_model_name("gpt-5.2:free"), "gpt-5.2:free");
    }

    #[test]
    fn normalize_config_aliases_override_builtin() {
        let mut aliases = std::collections::HashMap::new();
        aliases.insert(
            "sonnet".to_string(),
            "anthropic/claude-sonnet-4-5".to_string(),
        );
        assert_eq!(
            normalize_model_name_with_aliases("sonnet", &aliases),
            "anthropic/claude-sonnet-4-5"
        );
    }

    #[test]
    fn normalize_config_aliases_with_prefix_and_slash_value() {
        // Alias value contains `/` — used as-is, caller's prefix NOT prepended
        let mut aliases = std::collections::HashMap::new();
        aliases.insert("grok-latest".to_string(), "x-ai/grok-4.2".to_string());
        assert_eq!(
            normalize_model_name_with_aliases("openrouter/grok-latest", &aliases),
            "x-ai/grok-4.2"
        );
    }

    #[test]
    fn normalize_config_aliases_with_prefix_bare_value() {
        // Alias value is bare — caller's prefix IS prepended
        let mut aliases = std::collections::HashMap::new();
        aliases.insert("fast".to_string(), "gemini-3-flash-preview".to_string());
        assert_eq!(
            normalize_model_name_with_aliases("google/fast", &aliases),
            "google/gemini-3-flash-preview"
        );
    }

    #[test]
    fn normalize_config_aliases_with_suffix() {
        let mut aliases = std::collections::HashMap::new();
        aliases.insert(
            "sonnet".to_string(),
            "anthropic/claude-sonnet-4-5".to_string(),
        );
        assert_eq!(
            normalize_model_name_with_aliases("sonnet:free", &aliases),
            "anthropic/claude-sonnet-4-5:free"
        );
    }

    #[test]
    fn normalize_config_aliases_case_insensitive() {
        let mut aliases = std::collections::HashMap::new();
        aliases.insert(
            "Sonnet".to_string(),
            "anthropic/claude-sonnet-4-5".to_string(),
        );
        assert_eq!(
            normalize_model_name_with_aliases("sonnet", &aliases),
            "anthropic/claude-sonnet-4-5"
        );
    }

    #[test]
    fn resolve_max_output_tokens_explicit() {
        let config = Config::default();
        assert_eq!(
            resolve_max_output_tokens(Some(4096), &config, None, None),
            Some(4096)
        );
    }

    #[test]
    fn resolve_max_output_tokens_fallback() {
        let config = Config::default();
        assert_eq!(resolve_max_output_tokens(None, &config, None, None), None);
    }

    #[test]
    fn resolve_max_output_tokens_from_registry() {
        let config = Config::default();
        let mut registry = ModelRegistry::default();
        registry.models.push(yoetz_core::registry::ModelEntry {
            id: "gemini/gemini-3-pro-preview".to_string(),
            context_length: None,
            max_output_tokens: Some(65535),
            pricing: Default::default(),
            provider: None,
            capability: None,
            tier: None,
        });
        registry.rebuild_index();
        // Should cap at 16384
        assert_eq!(
            resolve_max_output_tokens(
                None,
                &config,
                Some(&registry),
                Some("gemini/gemini-3-pro-preview"),
            ),
            Some(16384)
        );
    }

    #[test]
    fn resolve_max_output_tokens_registry_small_model() {
        let config = Config::default();
        let mut registry = ModelRegistry::default();
        registry.models.push(yoetz_core::registry::ModelEntry {
            id: "test/small-model".to_string(),
            context_length: None,
            max_output_tokens: Some(4096),
            pricing: Default::default(),
            provider: None,
            capability: None,
            tier: None,
        });
        registry.rebuild_index();
        // Model max (4096) is less than cap (16384), so use model max
        assert_eq!(
            resolve_max_output_tokens(None, &config, Some(&registry), Some("test/small-model")),
            Some(4096)
        );
    }

    #[test]
    fn resolve_prompt_preserves_em_dash() {
        let input = "Summarize this — and that".to_string();
        let result = resolve_prompt(Some(input.clone()), None).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn resolve_prompt_preserves_unicode_dashes() {
        // em-dash U+2014, en-dash U+2013, horizontal bar U+2015, minus sign U+2212
        let input = "a\u{2014}b \u{2013} c \u{2015} d \u{2212} e".to_string();
        let result = resolve_prompt(Some(input.clone()), None).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn build_model_spec_auto_prefix_openrouter() {
        let mut registry = ModelRegistry::default();
        registry.models.push(yoetz_core::registry::ModelEntry {
            id: "x-ai/grok-4".to_string(),
            context_length: None,
            max_output_tokens: None,
            pricing: Default::default(),
            provider: Some("openrouter".to_string()),
            capability: None,
            tier: None,
        });
        registry.rebuild_index();
        let result = build_model_spec(None, "x-ai/grok-4", Some(&registry)).unwrap();
        assert_eq!(result, "openrouter/x-ai/grok-4");
    }

    #[test]
    fn build_model_spec_no_registry_passthrough() {
        let result = build_model_spec(None, "x-ai/grok-4", None).unwrap();
        assert_eq!(result, "x-ai/grok-4");
    }

    #[test]
    fn build_model_spec_not_in_registry_passthrough() {
        let registry = ModelRegistry::default();
        let result = build_model_spec(None, "unknown/model", Some(&registry)).unwrap();
        assert_eq!(result, "unknown/model");
    }

    #[test]
    fn build_model_spec_non_openrouter_no_prefix() {
        let mut registry = ModelRegistry::default();
        registry.models.push(yoetz_core::registry::ModelEntry {
            id: "gemini/gemini-3-pro-preview".to_string(),
            context_length: None,
            max_output_tokens: None,
            pricing: Default::default(),
            provider: Some("gemini".to_string()),
            capability: None,
            tier: None,
        });
        registry.rebuild_index();
        // Model with non-openrouter provider should NOT be auto-prefixed
        let result =
            build_model_spec(None, "gemini/gemini-3-pro-preview", Some(&registry)).unwrap();
        assert_eq!(result, "gemini/gemini-3-pro-preview");
    }

    #[test]
    fn build_model_spec_no_slash_no_prefix() {
        let mut registry = ModelRegistry::default();
        registry.models.push(yoetz_core::registry::ModelEntry {
            id: "gpt-5.2".to_string(),
            context_length: None,
            max_output_tokens: None,
            pricing: Default::default(),
            provider: Some("openrouter".to_string()),
            capability: None,
            tier: None,
        });
        registry.rebuild_index();
        // Model without slash should NOT be auto-prefixed even if in registry
        let result = build_model_spec(None, "gpt-5.2", Some(&registry)).unwrap();
        assert_eq!(result, "gpt-5.2");
    }

    #[test]
    fn read_text_file_truncates_utf8_safely() {
        let text = "hello 🙂 world";
        let bytes = text.as_bytes();
        let cut = bytes.iter().position(|b| *b == 0xF0).unwrap_or(bytes.len());
        let path = std::env::temp_dir().join(format!(
            "yoetz_read_text_{}.txt",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, bytes).unwrap();
        let (content, truncated) = read_text_file(&path, cut + 1).unwrap();
        assert!(truncated);
        assert!(content.starts_with("hello "));
        let _ = fs::remove_file(path);
    }
}

/// Validate a model ID against the registry.
///
/// - Exact match: pass.
/// - Fuzzy matches: error with "Did you mean?" suggestions.
/// - No matches at all: error with sync hint.
///
/// When `allow_unknown` is true, unknown models pass silently (for self-hosted models).
pub(crate) fn validate_model_or_suggest(
    model_id: &str,
    registry: Option<&yoetz_core::registry::ModelRegistry>,
    allow_unknown: bool,
) -> Result<()> {
    let Some(registry) = registry else {
        return Ok(());
    };
    // Exact match found — all good
    if registry.find(model_id).is_some() {
        return Ok(());
    }
    // Try fuzzy search
    let matches = fuzzy::fuzzy_search(registry, model_id, 3);
    if matches.is_empty() {
        if allow_unknown {
            return Ok(());
        }
        return Err(anyhow!(
            "model '{}' not found in registry.\n\
             Hint: run `yoetz models sync` to update the registry, \
             or use --allow-unknown for self-hosted models.",
            model_id,
        ));
    }
    let suggestions: Vec<String> = matches.iter().map(|m| m.id.clone()).collect();
    Err(anyhow!(
        "model '{}' not found in registry. Did you mean: {}?\n\
         Hint: run `yoetz models resolve {}` to search, or `yoetz models sync` to update the registry.",
        model_id,
        suggestions.join(", "),
        model_id,
    ))
}

async fn call_litellm(
    litellm: &LiteLLM,
    provider: Option<&str>,
    model: &str,
    prompt: &str,
    temperature: f32,
    max_output_tokens: Option<usize>,
    response_format: Option<Value>,
    images: &[MediaInput],
    video: Option<&MediaInput>,
) -> Result<CallResult> {
    let model_spec = build_model_spec(provider, model, None)?;
    let mut req = ChatRequest::new(model_spec).temperature(temperature);
    if let Some(max) = max_output_tokens {
        req = req.max_tokens(max as u32);
    }
    req.response_format = response_format;

    if images.is_empty() && video.is_none() {
        req = req.message("user", prompt);
    } else {
        let mut parts = Vec::new();
        parts.push(ChatContentPart::Text(ChatContentPartText {
            kind: std::borrow::Cow::Borrowed("text"),
            text: prompt.to_string(),
        }));
        for image in images {
            parts.push(media_to_image_part(image)?);
        }
        if let Some(video) = video {
            parts.push(media_to_file_part(video)?);
        }
        req = req.message_with_content("user", ChatMessageContent::Parts(parts));
    }

    let resp = litellm.completion(req).await?;
    Ok(CallResult {
        content: resp.content,
        usage: usage_from_litellm(resp.usage),
        response_id: resp.response_id,
        header_cost: resp.header_cost,
    })
}

/// Look up a model in the registry and return its provider if the model
/// contains a `/` (i.e. looks like `vendor/model`).
pub(crate) fn resolve_provider_from_registry(
    model: &str,
    registry: &ModelRegistry,
) -> Option<String> {
    if !model.contains('/') {
        return None;
    }
    let entry = registry.find(model)?;
    entry.provider.clone()
}

fn build_model_spec(
    provider: Option<&str>,
    model: &str,
    registry: Option<&ModelRegistry>,
) -> Result<String> {
    let Some(provider) = provider else {
        // If model has a slash and exists in registry as an openrouter model, auto-prefix
        if model.contains('/') {
            if let Some(reg) = registry {
                if let Some(entry) = reg.find(model) {
                    if entry.provider.as_deref() == Some("openrouter") {
                        return Ok(format!("openrouter/{model}"));
                    }
                }
            }
        }
        return Ok(model.to_string());
    };
    let provider_lc = provider.to_lowercase();
    if let Some((prefix, _rest)) = model.split_once('/') {
        let prefix_lc = prefix.to_lowercase();
        if provider_lc == "gemini" && prefix_lc == "models" {
            return Ok(format!("{provider}/{model}"));
        }
        if provider_lc == "openrouter" {
            if prefix_lc == "openrouter" {
                let rest = model.split_once('/').map(|(_, rest)| rest).unwrap_or("");
                if !rest.contains('/') {
                    return Err(anyhow!(
                        "openrouter models must be namespaced (e.g. openai/gpt-5.2, anthropic/claude-sonnet-4-5)"
                    ));
                }
                return Ok(model.to_string());
            }
            return Ok(format!("{provider}/{model}"));
        }
        if prefix_lc == provider_lc {
            return Ok(model.to_string());
        }
        return Err(anyhow!(
            "model prefix '{prefix}' conflicts with provider '{provider}'. \
use --provider {prefix} or pass an unprefixed model name"
        ));
    }
    if provider_lc == "openrouter" {
        return Err(anyhow!(
            "openrouter models must be namespaced (e.g. openai/gpt-5.2, anthropic/claude-sonnet-4-5)"
        ));
    }
    Ok(format!("{provider}/{model}"))
}

/// Built-in aliases (fallback when config has no matching `[aliases]` entry).
fn builtin_aliases() -> &'static [(&'static str, &'static str)] {
    &[
        ("gemini-pro-3", "gemini-3-pro-preview"),
        ("gemini-flash-3", "gemini-3-flash-preview"),
    ]
}

fn normalize_model_name_with_aliases(
    model: &str,
    config_aliases: &std::collections::HashMap<String, String>,
) -> String {
    let lower = model.to_lowercase();
    // Strip OpenRouter suffixes like :free, :extended before matching
    let (lower_base, suffix) = lower
        .rsplit_once(':')
        .map(|(b, s)| (b, format!(":{s}")))
        .unwrap_or((lower.as_str(), String::new()));

    // Extract the bare model name (after any provider prefix).
    // Generic: splits on the last `/` boundary to handle any `provider/model` form,
    // with special handling for multi-segment prefixes like `openrouter/google/`.
    let (prefix, bare) = if let Some(pos) = lower_base.rfind('/') {
        (&lower_base[..=pos], &lower_base[pos + 1..])
    } else {
        ("", lower_base)
    };

    // Look up in config aliases first, then built-in aliases
    let resolved = config_aliases
        .iter()
        .find(|(k, _)| k.to_lowercase() == bare)
        .map(|(_, v)| v.as_str())
        .or_else(|| {
            builtin_aliases()
                .iter()
                .find(|(k, _)| *k == bare)
                .map(|(_, v)| *v)
        });

    match resolved {
        Some(replacement) => {
            // If the alias value already contains a `/` (e.g. "anthropic/claude-sonnet-4-5"),
            // use it as-is — the user specified the full path. Only prepend the caller's
            // prefix for bare replacement values (e.g. "gemini-3-pro-preview").
            if replacement.contains('/') {
                format!("{replacement}{suffix}")
            } else {
                format!("{prefix}{replacement}{suffix}")
            }
        }
        None => model.to_string(),
    }
}

/// Resolve max output tokens. Returns `None` when no explicit limit is set,
/// letting each provider use its own model-default maximum.
fn resolve_max_output_tokens(
    requested: Option<usize>,
    config: &Config,
    registry: Option<&ModelRegistry>,
    model_id: Option<&str>,
) -> Option<usize> {
    if let Some(v) = requested {
        return Some(v);
    }
    if let Some(v) = config.defaults.max_output_tokens {
        return Some(v);
    }
    if let (Some(reg), Some(id)) = (registry, model_id) {
        if let Some(entry) = reg.find(id) {
            if let Some(model_max) = entry.max_output_tokens {
                return Some(model_max.min(REGISTRY_OUTPUT_TOKENS_CAP));
            }
        }
    }
    None
}

fn resolve_registry_model_id(
    provider: Option<&str>,
    model_id: Option<&str>,
    registry: Option<&ModelRegistry>,
) -> Option<String> {
    let model_id = model_id?;
    let mut candidates = Vec::new();
    candidates.push(model_id.to_string());

    if let Some(stripped) = model_id.strip_prefix("openrouter/") {
        candidates.push(stripped.to_string());
    }
    if let Some(stripped) = model_id.strip_prefix("models/") {
        candidates.push(stripped.to_string());
    }

    if let Some(provider) = provider {
        let provider_lc = provider.to_lowercase();
        if !model_id.contains('/') {
            candidates.push(format!("{provider}/{model_id}"));
            if provider_lc == "gemini" {
                candidates.push(format!("google/{model_id}"));
            }
        }
    }

    if let Some(registry) = registry {
        for candidate in &candidates {
            if registry.find(candidate).is_some() {
                return Some(candidate.clone());
            }
        }
    }

    candidates.into_iter().next()
}

/// Convert litellm_rust::Usage to yoetz_core::types::Usage.
///
/// Both types now use u64 for token counts, so this is a straightforward mapping.
fn usage_from_litellm(usage: litellm_rust::Usage) -> Usage {
    Usage {
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
        thoughts_tokens: usage.thoughts_tokens,
        total_tokens: usage.total_tokens,
        cost_usd: usage.cost_usd,
    }
}

fn apply_capability_warnings(
    registry: Option<&ModelRegistry>,
    model_id: Option<&str>,
    has_images: bool,
    has_video: bool,
    pricing: &mut PricingEstimate,
) -> Result<()> {
    if !has_images && !has_video {
        return Ok(());
    }
    let Some(model_id) = model_id else {
        return Ok(());
    };
    let Some(registry) = registry else {
        pricing.warnings.push(
            "registry unavailable; cannot validate model capabilities (run `yoetz models sync`)"
                .to_string(),
        );
        return Ok(());
    };
    let Some(entry) = registry.find(model_id) else {
        pricing.warnings.push(format!(
            "model capabilities unknown; {model_id} not in registry (run `yoetz models sync`)"
        ));
        return Ok(());
    };

    if has_images {
        match entry.capability.as_ref().and_then(|cap| cap.vision) {
            Some(true) => {}
            Some(false) => {
                return Err(anyhow!("model {model_id} does not support image inputs"));
            }
            None => pricing.warnings.push(format!(
                "model capability unknown for {model_id}; cannot validate vision inputs"
            )),
        }
    }

    if has_video {
        pricing.warnings.push(
            "video support is not tracked in registry; provider gemini is required".to_string(),
        );
    }

    Ok(())
}

fn media_to_image_part(media: &MediaInput) -> Result<ChatContentPart> {
    if media.media_type != yoetz_core::media::MediaType::Image {
        return Err(anyhow!("expected image media input"));
    }
    let url = media.as_data_url()?;
    Ok(ChatContentPart::ImageUrl(ChatContentPartImageUrl {
        kind: std::borrow::Cow::Borrowed("image_url"),
        image_url: ChatImageUrl::Url(url),
    }))
}

fn media_to_file_part(media: &MediaInput) -> Result<ChatContentPart> {
    let url = media.as_data_url()?;
    Ok(ChatContentPart::File(ChatContentPartFile {
        kind: std::borrow::Cow::Borrowed("file"),
        file: ChatFile {
            file_id: None,
            file_data: Some(url),
            format: Some(media.mime_type.clone()),
            detail: None,
            video_metadata: None,
        },
    }))
}

async fn save_image_outputs(
    client: &reqwest::Client,
    images: Vec<ImageData>,
    output_dir: &std::path::Path,
    model: &str,
) -> Result<Vec<yoetz_core::media::MediaOutput>> {
    let mut outputs = Vec::new();
    for (idx, image) in images.into_iter().enumerate() {
        let ext = match image.mime_type.as_deref() {
            Some("image/jpeg") => "jpg",
            Some("image/webp") => "webp",
            _ => "png",
        };
        let filename = format!("image_{idx}.{ext}");
        let path = output_dir.join(filename);
        if let Some(b64) = image.b64_json.as_ref() {
            let bytes = general_purpose::STANDARD
                .decode(b64.as_bytes())
                .context("decode image base64")?;
            std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
        } else if let Some(url) = image.url.as_ref() {
            const MAX_IMAGE_BYTES: u64 = 50 * 1024 * 1024; // 50 MB
            let resp = client.get(url).send().await?.error_for_status()?;
            if let Some(ct) = resp.headers().get(reqwest::header::CONTENT_TYPE) {
                let ct_str = ct.to_str().unwrap_or("");
                if !ct_str.starts_with("image/") {
                    eprintln!("warning: image download content-type is {ct_str}, expected image/*");
                }
            }
            if let Some(cl) = resp.content_length() {
                if cl > MAX_IMAGE_BYTES {
                    anyhow::bail!("image download too large ({cl} bytes, max {MAX_IMAGE_BYTES})");
                }
            }
            let bytes = resp.bytes().await?;
            if bytes.len() as u64 > MAX_IMAGE_BYTES {
                anyhow::bail!(
                    "image download too large ({} bytes, max {MAX_IMAGE_BYTES})",
                    bytes.len()
                );
            }
            std::fs::write(&path, &bytes).with_context(|| format!("write {}", path.display()))?;
        } else {
            continue;
        }

        outputs.push(yoetz_core::media::MediaOutput {
            media_type: yoetz_core::media::MediaType::Image,
            path,
            url: image.url,
            metadata: yoetz_core::media::MediaMetadata {
                width: None,
                height: None,
                duration_secs: None,
                model: model.to_string(),
                revised_prompt: image.revised_prompt,
            },
        });
    }
    Ok(outputs)
}

async fn fetch_openrouter_cost(
    client: &reqwest::Client,
    config: &Config,
    response_id: &str,
) -> Result<Option<f64>> {
    let provider_cfg = config.providers.get("openrouter");
    let base_url = provider_cfg
        .and_then(|p| p.base_url.clone())
        .or_else(|| providers::default_base_url("openrouter"))
        .ok_or_else(|| anyhow!("base_url not found for openrouter"))?;

    let api_key_env = provider_cfg
        .and_then(|p| p.api_key_env.clone())
        .or_else(|| providers::default_api_key_env("openrouter"))
        .unwrap_or_else(|| "OPENROUTER_API_KEY".to_string());

    let api_key = match env::var(&api_key_env) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    let url = format!(
        "{}/generation?id={}",
        base_url.trim_end_matches('/'),
        response_id
    );

    let (payload, _) = send_json::<Value>(client.get(url).bearer_auth(api_key)).await?;
    let data = payload.get("data").unwrap_or(&Value::Null);
    Ok(parse_cost(data.get("total_cost"))
        .or_else(|| parse_cost(data.get("total_cost_usd")))
        .or_else(|| parse_cost(payload.get("total_cost"))))
}

fn parse_cost(value: Option<&Value>) -> Option<f64> {
    let v = value?;
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    if let Some(s) = v.as_str() {
        return s.parse::<f64>().ok();
    }
    None
}

// defaults moved to providers module
