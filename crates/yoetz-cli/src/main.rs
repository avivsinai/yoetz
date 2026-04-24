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
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

mod browser;
mod budget;
mod chatgpt_recipe;
mod chatgpt_web;
mod chrome_devtools_mcp;
mod commands;
mod dev_browser;
mod fuzzy;
mod http;
mod live_attach;
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

    #[arg(long = "config-profile", global = true)]
    config_profile: Option<String>,

    #[arg(long, global = true, default_value = "180")]
    timeout_secs: u64,

    /// Allow unrecognized model IDs (for self-hosted models not in the registry)
    #[arg(long, global = true)]
    allow_unknown: bool,

    #[command(subcommand)]
    command: Commands,
}

struct AppContext {
    config: Config,
    browser_defaults: browser::BrowserDefaults,
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

    #[arg(long, help = "Include dotfiles and dot-directories; implied by --all")]
    include_hidden: bool,
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
    Doctor(BrowserDoctorArgs),
    /// Explicitly reset browser automation daemons. Use this when recovery is
    /// needed; yoetz does not silently recycle live-attach daemons for you.
    Reset(BrowserResetArgs),
    /// Sync cookies from Chrome to agent-browser (bypasses Cloudflare)
    SyncCookies(BrowserSyncCookiesArgs),
    /// Attach to a running Chrome instance and verify authentication.
    /// Auto-discovers via chrome://inspect (Chrome 144+), or use --cdp for explicit endpoint.
    Attach(BrowserAttachArgs),
    /// Thin CDP smoke-test for CI/integration: attaches to the given CDP
    /// endpoint, opens an `about:blank` tab, and reports JSON success. Does
    /// not navigate to ChatGPT, does not probe authentication — so it is
    /// safe to run against a fresh throwaway Chrome for Testing instance.
    VerifyCdp(BrowserVerifyCdpArgs),
    #[command(hide = true, name = "live-attach-daemon")]
    LiveAttachDaemon(BrowserLiveAttachDaemonArgs),
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

    /// Select a local Chrome browser by its published `/devtools/browser/<id>` suffix.
    #[arg(long)]
    browser_id: Option<String>,

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
    /// Select a local Chrome browser by its published `/devtools/browser/<id>` suffix.
    #[arg(long)]
    browser_id: Option<String>,
}

#[derive(Args)]
struct BrowserCheckArgs {
    #[arg(long)]
    profile: Option<PathBuf>,
    /// CDP endpoint (e.g. http://127.0.0.1:9222). Falls back to YOETZ_BROWSER_CDP env
    /// or config, then chrome://inspect auto-connect (Chrome 144+).
    #[arg(long)]
    cdp: Option<String>,
    /// Select a local Chrome browser by its published `/devtools/browser/<id>` suffix.
    #[arg(long)]
    browser_id: Option<String>,
}

#[derive(Args)]
struct BrowserAttachArgs {
    /// CDP endpoint (e.g. http://127.0.0.1:9222). Falls back to YOETZ_BROWSER_CDP env
    /// or config, then chrome://inspect auto-connect (Chrome 144+).
    #[arg(long)]
    cdp: Option<String>,
    /// Select a local Chrome browser by its published `/devtools/browser/<id>` suffix.
    #[arg(long)]
    browser_id: Option<String>,
}

#[derive(Args)]
struct BrowserLiveAttachDaemonArgs {}

#[derive(Args)]
struct BrowserVerifyCdpArgs {
    /// CDP endpoint (e.g. http://127.0.0.1:9222). Required — this command
    /// exists to verify CI-launched Chrome for Testing instances, so it does
    /// not fall back to auto-discovery.
    #[arg(long)]
    cdp: String,
    /// Verification page URL (default `about:blank`). Must be a
    /// yoetz-safe / throwaway URL — this subcommand will navigate to it.
    #[arg(long, default_value = "about:blank")]
    url: String,
}

#[derive(Args)]
struct BrowserDoctorArgs {
    /// Perform a live auto-connect probe against Chrome. This may trigger
    /// Chrome's remote-debugging approval dialog.
    #[arg(long)]
    live: bool,
}

#[derive(Args)]
struct BrowserResetArgs {}

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    errors: Vec<CouncilModelError>,
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
struct CouncilModelError {
    model: String,
    provider: String,
    error: String,
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

const BASE_PROTECTED_DOTENV_ENV_VARS: &[&str] = &[
    "YOETZ_AGENT_BROWSER_BIN",
    "YOETZ_DEV_BROWSER_BIN",
    "YOETZ_SCRIPTS_DIR",
    "YOETZ_CONFIG_PATH",
    "YOETZ_REGISTRY_PATH",
    "YOETZ_BROWSER_CDP",
    "YOETZ_BROWSER_TARGET_PATH",
    "YOETZ_BROWSER_PROFILE",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "GEMINI_API_KEY",
    "OPENROUTER_API_KEY",
    "XAI_API_KEY",
    "LITELLM_API_KEY",
];

fn protected_dotenv_env_vars(config: &Config) -> Vec<String> {
    let mut keys = BASE_PROTECTED_DOTENV_ENV_VARS
        .iter()
        .map(|key| (*key).to_string())
        .collect::<std::collections::BTreeSet<_>>();
    for provider in config.providers.values() {
        if let Some(key) = provider
            .api_key_env
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            keys.insert(key.to_string());
        }
    }
    keys.into_iter().collect()
}

fn snapshot_protected_dotenv_env(keys: &[String]) -> Vec<(String, Option<String>)> {
    keys.iter()
        .map(|key| (key.clone(), env::var(key).ok()))
        .collect()
}

fn restore_protected_dotenv_env(snapshot: &[(String, Option<String>)]) {
    for (key, pre_value) in snapshot {
        let post_value = env::var(key).ok();
        if post_value != *pre_value {
            match pre_value {
                Some(value) => env::set_var(key, value),
                None => env::remove_var(key),
            }
            eprintln!("warning: CWD .env tried to override {key}, ignored");
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load_with_profile(cli.config_profile.as_deref())?;
    let browser_defaults =
        browser::load_browser_defaults_with_profile(cli.config_profile.as_deref())?;

    // Capture security-sensitive env vars before dotenv loading.
    // CWD .env files must not override executable paths (supply-chain risk)
    // or redirect config, registry, browser targets, or API keys.
    let protected_env_keys = protected_dotenv_env_vars(&config);
    let protected_env = snapshot_protected_dotenv_env(&protected_env_keys);

    // Load environment files (.env.local takes precedence over .env)
    dotenvy::from_filename(".env.local").ok();
    dotenvy::dotenv().ok();

    restore_protected_dotenv_env(&protected_env);
    let format = resolve_format(cli.format.as_deref())?;

    if cli.debug {
        env::set_var("YOETZ_GEMINI_DEBUG", "1");
        env::set_var("LITELLM_GEMINI_DEBUG", "1");
        // Unlock detailed CDP error rendering (inferred emails + sample tabs)
        // when the user explicitly asks for debug output. Default remains
        // redacted to avoid leaking browsing context in routine errors
        // (review finding #9).
        env::set_var(chrome_devtools_mcp::client::YOETZ_DEBUG_CDP_ENV, "1");
    }
    let client = build_client(cli.timeout_secs)?;
    let litellm = std::sync::Arc::new(build_litellm(&config, client.clone())?);
    let ctx = AppContext {
        config,
        browser_defaults,
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
        Commands::Browser(args) => handle_browser(&ctx, args, format).await,
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

fn is_chatgpt_recipe(recipe: &browser::Recipe, recipe_path: &Path) -> bool {
    recipe
        .name
        .as_deref()
        .is_some_and(|name| name.eq_ignore_ascii_case("chatgpt"))
        || recipe_path
            .to_string_lossy()
            .to_lowercase()
            .contains("chatgpt")
}

fn recipe_transport_name(transport: browser::RecipeTransport) -> &'static str {
    match transport {
        browser::RecipeTransport::DevBrowser => "dev-browser",
        browser::RecipeTransport::AgentBrowser => "agent-browser",
        browser::RecipeTransport::ChromeDevtoolsMcp => "chrome-devtools-mcp",
        browser::RecipeTransport::Manual => "manual",
    }
}

fn manual_browser_recipe_fallback(recipe_path: &Path, bundle: Option<&Path>) -> String {
    let bundle_hint = bundle
        .map(|path| format!(" Upload or paste `{}` manually.", path.display()))
        .unwrap_or_default();
    format!(
        "manual fallback for `{}`: open ChatGPT in the target Chrome profile and complete the web flow manually.{bundle_hint}",
        recipe_path.display()
    )
}

fn format_recipe_transport_errors(errors: &[(browser::RecipeTransport, String)]) -> String {
    let joined = errors
        .iter()
        .map(|(transport, detail)| format!("{}: {detail}", recipe_transport_name(*transport)))
        .collect::<Vec<_>>()
        .join("\n- ");
    format!("all browser recipe transports failed:\n- {joined}")
}

fn recipe_transport_error_detail(err: &anyhow::Error) -> String {
    format!("{err:#}")
}

fn recipe_has_remaining_manual_fallback(
    transports: &[browser::RecipeTransport],
    current_index: usize,
) -> bool {
    transports[current_index + 1..]
        .iter()
        .copied()
        .any(|next| matches!(next, browser::RecipeTransport::Manual))
}

fn recipe_should_stop_live_transport_fallback(
    err: &anyhow::Error,
    selected_cdp_target: Option<&browser::ResolvedCdpTarget>,
    transport: browser::RecipeTransport,
    recipe_vars: &std::collections::BTreeMap<String, String>,
) -> bool {
    if live_attach::is_daemon_rpc_timeout_error(err) {
        return true;
    }
    if browser::is_chrome_approval_wait_error(err) {
        return true;
    }
    if browser::is_chatgpt_auth_issue_error(err) {
        if recipe_uses_exact_browser_context_selector(recipe_vars) {
            return true;
        }
        if selected_cdp_target.is_some_and(browser::ResolvedCdpTarget::is_authoritative) {
            return true;
        }
        return !is_live_cdp_only_transport(transport);
    }
    if browser::is_chatgpt_profile_selector_visibility_error(err) {
        if recipe_uses_exact_browser_context_selector(recipe_vars) {
            return true;
        }
        if recipe_uses_profile_email_selector(recipe_vars) {
            if selected_cdp_target.is_some_and(browser::ResolvedCdpTarget::is_authoritative) {
                return true;
            }
            return !matches!(
                transport,
                browser::RecipeTransport::ChromeDevtoolsMcp | browser::RecipeTransport::DevBrowser
            );
        }
    }
    if browser::is_chatgpt_attached_page_error(err) {
        if recipe_uses_exact_browser_context_selector(recipe_vars) {
            return true;
        }
        if selected_cdp_target.is_some_and(browser::ResolvedCdpTarget::is_authoritative) {
            return true;
        }
        return matches!(transport, browser::RecipeTransport::AgentBrowser);
    }

    // Once the user has explicitly pinned a specific live Chrome target, do not
    // fan out into more browser transports after the first failure. Env/config
    // targets and auto-selected targets remain advisory.
    selected_cdp_target.is_some_and(browser::ResolvedCdpTarget::is_authoritative)
        && !matches!(transport, browser::RecipeTransport::Manual)
}

/// A transport is "pure live-CDP" if its only way to drive the browser is
/// attaching to a running Chrome over CDP. `chrome-devtools-mcp` (vendored
/// headless_chrome) and `dev-browser` (Playwright `connectOverCDP`) are
/// pure live-CDP. `agent-browser` is NOT — when live-attach fails, it
/// transparently falls back to a managed profile with stored cookies, so
/// it still works on Chrome 146+ default profiles where CDP is unreachable.
/// `manual` never needs CDP.
fn is_live_cdp_only_transport(transport: browser::RecipeTransport) -> bool {
    matches!(
        transport,
        browser::RecipeTransport::ChromeDevtoolsMcp | browser::RecipeTransport::DevBrowser
    )
}

/// When tier 1 (chrome-devtools-mcp) already determined Chrome is not
/// listening on CDP, any other pure live-CDP transport will fail for the
/// same reason — and dev-browser's Playwright `connectOverCDP` in
/// particular hangs on `Target.setAutoAttach` instead of failing fast.
/// Skip those tiers but still let agent-browser try (it can fall back to
/// managed profile without CDP).
fn recipe_should_skip_remaining_live_cdp_transports(err: &anyhow::Error) -> bool {
    browser::is_chrome_cdp_unreachable_error(err)
}

fn recipe_var_present(
    recipe_vars: &std::collections::BTreeMap<String, String>,
) -> impl Fn(&str) -> bool + '_ {
    move |key| {
        recipe_vars
            .get(key)
            .is_some_and(|value| !value.trim().is_empty())
    }
}

fn recipe_uses_exact_browser_context_selector(
    recipe_vars: &std::collections::BTreeMap<String, String>,
) -> bool {
    recipe_var_present(recipe_vars)("browser_context_id")
}

fn recipe_uses_profile_email_selector(
    recipe_vars: &std::collections::BTreeMap<String, String>,
) -> bool {
    recipe_var_present(recipe_vars)("profile_email")
}

#[cfg(test)]
fn recipe_uses_chatgpt_browser_context_selector(
    recipe_vars: &std::collections::BTreeMap<String, String>,
) -> bool {
    recipe_uses_exact_browser_context_selector(recipe_vars)
        || recipe_uses_profile_email_selector(recipe_vars)
}

fn constrain_chatgpt_transports_for_browser_context_selector(
    transports: Vec<browser::RecipeTransport>,
    recipe_vars: &std::collections::BTreeMap<String, String>,
    is_chatgpt: bool,
) -> Vec<browser::RecipeTransport> {
    if !is_chatgpt {
        return transports;
    }

    if recipe_uses_exact_browser_context_selector(recipe_vars) {
        return transports
            .into_iter()
            .filter(|transport| {
                matches!(
                    transport,
                    browser::RecipeTransport::ChromeDevtoolsMcp | browser::RecipeTransport::Manual
                )
            })
            .collect();
    }

    if recipe_uses_profile_email_selector(recipe_vars) {
        return transports
            .into_iter()
            .filter(|transport| {
                matches!(
                    transport,
                    browser::RecipeTransport::ChromeDevtoolsMcp
                        | browser::RecipeTransport::AgentBrowser
                        | browser::RecipeTransport::Manual
                )
            })
            .collect();
    }

    transports
}

fn live_attach_owner_present(summary: &live_attach::DaemonSummary) -> bool {
    matches!(summary.health, live_attach::DaemonHealth::Busy)
        || matches!(summary.health, live_attach::DaemonHealth::Healthy) && summary.session_count > 0
}

fn should_prefer_running_profile_auto_connect(
    selected_cdp_target: Option<&browser::ResolvedCdpTarget>,
    live_attach_owner_is_present: bool,
) -> bool {
    // No healthy raw CDP target was selected, so prefer the running-profile
    // transports before asking Chrome for a fresh live attach, unless yoetz
    // already has a live-attach owner for the implicit/default path.
    selected_cdp_target.is_none() && !live_attach_owner_is_present
}

fn maybe_print_running_profile_auto_connect_preference(
    prefer_auto_connect: bool,
    format: OutputFormat,
) {
    if prefer_auto_connect && matches!(format, OutputFormat::Text | OutputFormat::Markdown) {
        eprintln!(
            "info: no healthy raw Chrome DevTools target was discovered; reusing the running-profile auto-connect path instead of requesting a new raw CDP attach"
        );
    }
}

fn running_profile_recipe_transport_priority(transport: browser::RecipeTransport) -> u8 {
    match transport {
        browser::RecipeTransport::ChromeDevtoolsMcp => 0,
        browser::RecipeTransport::DevBrowser => 1,
        browser::RecipeTransport::AgentBrowser => 2,
        browser::RecipeTransport::Manual => 3,
    }
}

fn prioritize_chatgpt_transports_for_running_profile_auto_connect(
    transports: Vec<browser::RecipeTransport>,
    prefer_auto_connect: bool,
) -> Vec<browser::RecipeTransport> {
    if !prefer_auto_connect {
        return transports;
    }

    let has_dev_browser = transports.contains(&browser::RecipeTransport::DevBrowser);
    let has_chrome_devtools_mcp = transports.contains(&browser::RecipeTransport::ChromeDevtoolsMcp);
    let has_agent_browser = transports.contains(&browser::RecipeTransport::AgentBrowser);
    let has_manual = transports.contains(&browser::RecipeTransport::Manual);
    if has_chrome_devtools_mcp {
        let mut constrained = vec![browser::RecipeTransport::ChromeDevtoolsMcp];
        if has_dev_browser {
            constrained.push(browser::RecipeTransport::DevBrowser);
        }
        if has_manual {
            constrained.push(browser::RecipeTransport::Manual);
        }
        return constrained;
    }
    if has_dev_browser {
        let mut constrained = vec![browser::RecipeTransport::DevBrowser];
        if has_manual {
            constrained.push(browser::RecipeTransport::Manual);
        }
        return constrained;
    }
    if has_agent_browser {
        let mut constrained = vec![browser::RecipeTransport::AgentBrowser];
        if has_manual {
            constrained.push(browser::RecipeTransport::Manual);
        }
        return constrained;
    }

    let mut transports = transports;
    transports.sort_by_key(|transport| running_profile_recipe_transport_priority(*transport));
    transports
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrowserCheckTransport {
    ChromeDevtoolsMcp,
    DevBrowser,
    AgentBrowser,
}

fn browser_check_transports(
    dev_browser_available: bool,
    managed_profile_only: bool,
    prefer_auto_connect: bool,
) -> Vec<BrowserCheckTransport> {
    if managed_profile_only {
        return vec![BrowserCheckTransport::AgentBrowser];
    }

    if prefer_auto_connect {
        let mut transports = vec![BrowserCheckTransport::ChromeDevtoolsMcp];
        if dev_browser_available {
            transports.push(BrowserCheckTransport::DevBrowser);
        }
        return transports;
    }

    let mut transports = vec![BrowserCheckTransport::ChromeDevtoolsMcp];
    if dev_browser_available {
        transports.push(BrowserCheckTransport::DevBrowser);
    }
    transports.push(BrowserCheckTransport::AgentBrowser);
    transports
}

fn browser_check_transport_name(transport: BrowserCheckTransport) -> &'static str {
    match transport {
        BrowserCheckTransport::ChromeDevtoolsMcp => "chrome-devtools-mcp",
        BrowserCheckTransport::DevBrowser => "dev-browser",
        BrowserCheckTransport::AgentBrowser => "agent-browser",
    }
}

fn browser_check_live_method(target: Option<&browser::ResolvedCdpTarget>) -> String {
    match target {
        Some(target) if !target.is_auto_discovered() => format!("cdp: {}", target.endpoint),
        _ => "auto_connect".to_string(),
    }
}

fn remember_browser_check_live_attach_failure(slot: &mut Option<String>, err: &anyhow::Error) {
    if slot.is_none() && browser::is_chrome_cdp_unreachable_error(err) {
        *slot = Some(format!("{err:#}"));
    }
}

fn live_attach_daemon_timeout_fallback_error(action: &str, err: anyhow::Error) -> anyhow::Error {
    err.context(format!(
        "yoetz live-attach daemon did not finish the {action} request within its operation window. Leaving the live owner intact instead of falling through to another browser transport. If this repeats, run `yoetz browser reset`."
    ))
}

fn maybe_prefer_browser_check_live_attach_failure(
    err: anyhow::Error,
    prior_live_attach_failure: Option<&str>,
) -> anyhow::Error {
    if browser::is_chatgpt_auth_issue_error(&err) {
        if let Some(prior) = prior_live_attach_failure {
            return anyhow!(
                "live Chrome attach failed before managed fallback could verify ChatGPT auth.\n\nLive-attach error: {prior}\n\nManaged fallback error: {err}"
            );
        }
    }
    err
}

fn browser_check_exhausted_error(
    errors: &[(BrowserCheckTransport, String)],
    prior_live_attach_failure: Option<&str>,
) -> anyhow::Error {
    let attempted = errors
        .iter()
        .map(|(transport, detail)| {
            format!("{}: {detail}", browser_check_transport_name(*transport))
        })
        .collect::<Vec<_>>();
    let attempted = if attempted.is_empty() {
        "none".to_string()
    } else {
        attempted.join("\n- ")
    };

    if let Some(prior) = prior_live_attach_failure {
        return anyhow!(
            "browser check failed; no browser check transport succeeded.\n\n\
             Live-attach error: {prior}\n\n\
             Attempted transports:\n- {attempted}"
        );
    }

    anyhow!(
        "browser check failed; no browser check transport succeeded.\n\n\
         Attempted transports:\n- {attempted}"
    )
}

fn maybe_print_auto_selected_cdp_target(
    target: Option<&browser::ResolvedCdpTarget>,
    format: OutputFormat,
) {
    if !matches!(format, OutputFormat::Text | OutputFormat::Markdown) {
        return;
    }
    let Some(target) = target else {
        return;
    };
    if target.is_auto_discovered() {
        eprintln!("info: {}", target.description);
        if let Some(warning) = browser::auto_discovered_cdp_target_warning(target) {
            eprintln!("warning: {warning}");
        }
    }
}

fn explicit_cdp_attach_failure(err: anyhow::Error) -> anyhow::Error {
    if browser::is_chrome_approval_wait_error(&err) {
        anyhow!(
            "Chrome may be showing an \"Allow remote debugging?\" dialog. \
             Click Allow, then retry."
        )
    } else {
        err.context("explicit --cdp failed; not falling back")
    }
}

fn configured_cdp_attach_failure(err: anyhow::Error) -> anyhow::Error {
    if browser::is_chrome_approval_wait_error(&err) {
        anyhow!(
            "Chrome may be showing an \"Allow remote debugging?\" dialog. \
             Click Allow, then retry."
        )
    } else {
        err.context("configured CDP target failed; not falling back")
    }
}

fn resolved_cdp_attach_failure(
    err: anyhow::Error,
    target: &browser::ResolvedCdpTarget,
) -> anyhow::Error {
    match target.source {
        browser::ResolvedCdpTargetSource::Flag => explicit_cdp_attach_failure(err),
        browser::ResolvedCdpTargetSource::Env | browser::ResolvedCdpTargetSource::Config => {
            configured_cdp_attach_failure(err)
        }
        browser::ResolvedCdpTargetSource::Auto => {
            if browser::is_chrome_approval_wait_error(&err) {
                anyhow!(
                    "Chrome may be showing an \"Allow remote debugging?\" dialog. \
                     Click Allow, then retry."
                )
            } else {
                err.context("selected running Chrome target failed")
            }
        }
    }
}

fn profile_forces_managed_browser(
    profile: Option<&Path>,
    cdp_override: Option<&str>,
    browser_id: Option<&str>,
) -> bool {
    profile.is_some()
        && cdp_override.is_none()
        && browser_id.is_none_or(|value| value.trim().is_empty())
}

fn maybe_demote_auto_selected_cdp_target(
    target: &mut Option<browser::ResolvedCdpTarget>,
    format: OutputFormat,
    err: &anyhow::Error,
) {
    let Some(selected) = target.as_ref() else {
        return;
    };
    if !selected.is_auto_discovered() {
        return;
    }
    if !should_demote_auto_selected_cdp_target(err) {
        return;
    }

    let description = selected.description.clone();
    if let Err(clear_err) = browser::forget_cdp_target(selected) {
        eprintln!("warning: failed to clear auto-selected Chrome target after error: {clear_err}");
    }
    *target = None;

    if matches!(format, OutputFormat::Text | OutputFormat::Markdown) {
        eprintln!("info: {description} failed ({err}); continuing with fallback discovery");
    }
}

fn should_demote_auto_selected_cdp_target(err: &anyhow::Error) -> bool {
    if browser::is_chrome_approval_wait_error(err) {
        return false;
    }
    if browser::is_chatgpt_auth_issue_error(err) {
        return true;
    }
    if browser::is_chatgpt_profile_selector_visibility_error(err) {
        return true;
    }
    if browser::is_chatgpt_attached_page_error(err) {
        return true;
    }
    if browser::is_chrome_cdp_unreachable_error(err) {
        return true;
    }
    let message = format!("{err:#}");
    let message_lower = message.to_lowercase();
    if message_lower.contains("could not reach chrome's cdp endpoint") {
        return true;
    }
    if message.contains("selected running Chrome target failed") {
        return true;
    }
    dev_browser::is_dev_browser_connect_failure(err)
}

fn maybe_remember_cdp_target(target: Option<&browser::ResolvedCdpTarget>, format: OutputFormat) {
    let Some(target) = target else {
        return;
    };
    if let Err(err) = browser::remember_cdp_target(target) {
        if matches!(format, OutputFormat::Text | OutputFormat::Markdown) {
            eprintln!("warning: failed to persist last successful Chrome target: {err}");
        }
    }
}

fn default_daemon_recovery_error(original: Option<&anyhow::Error>) -> Option<anyhow::Error> {
    let suffix = original
        .map(|err| format!("\n\nOriginal error: {err}"))
        .unwrap_or_default();
    match browser::inspect_default_daemon() {
        browser::DaemonState::AwaitingApproval => Some(anyhow!(
            "Chrome may be showing an \"Allow remote debugging?\" dialog. Click Allow, then retry.{suffix}"
        )),
        browser::DaemonState::Stale => Some(anyhow!(
            "The agent-browser default daemon looks stale. Run `yoetz browser reset` and retry.{suffix}"
        )),
        browser::DaemonState::NoSocket | browser::DaemonState::Healthy => None,
    }
}

async fn run_recipe_via_chrome_devtools_mcp(
    ctx: &AppContext,
    recipe_args: &BrowserRecipeArgs,
    recipe_vars: &BTreeMap<String, String>,
    selected_cdp_target: Option<&browser::ResolvedCdpTarget>,
    format: OutputFormat,
    is_chatgpt: bool,
) -> Result<Value> {
    if !is_chatgpt {
        return Err(anyhow!(
            "chrome-devtools-mcp transport currently supports only the built-in `chatgpt` recipe; \
             claude and gemini ports will land after the chatgpt path is verified end-to-end"
        ));
    }
    if recipe_args.profile.is_some() {
        return Err(anyhow!(
            "chrome-devtools-mcp transport does not support `--profile`; \
             use `--cdp` to target a specific Chrome instance or omit both for default auto-connect"
        ));
    }
    if recipe_vars
        .get("paste")
        .is_some_and(|value| value == "true")
    {
        return Err(anyhow!(
            "chrome-devtools-mcp transport does not support paste mode; file attachment upload is required"
        ));
    }
    if recipe_args.bundle.is_none() {
        return Err(anyhow!(
            "chrome-devtools-mcp transport requires `--bundle`; it does not support inline paste mode"
        ));
    }

    let recipe_spec = build_chatgpt_recipe_spec(recipe_args, recipe_vars)?;
    let recipe_ctx = chrome_devtools_mcp::DevtoolsMcpRecipeContext {
        cdp_endpoint: selected_cdp_target.map(|target| target.endpoint.clone()),
        bundle_path: recipe_spec.bundle_path.clone(),
        bundle_text: None,
        model: recipe_spec.model.clone(),
        prompt: recipe_spec.prompt.clone(),
        browser_context_id: recipe_spec.browser_context_id.clone(),
        profile_email: recipe_spec.profile_email.clone(),
        run_id: recipe_spec.run_id.clone(),
        response_timeout_ms: recipe_spec.wait_timeout_ms,
        response_poll_interval_ms: recipe_spec.wait_interval_ms,
        disable_extended: recipe_spec.disable_extended,
        show_approval_guidance: matches!(format, OutputFormat::Text | OutputFormat::Markdown),
    };

    let response = live_attach::run_chatgpt_recipe(
        selected_cdp_target,
        recipe_ctx,
        matches!(format, OutputFormat::Text | OutputFormat::Markdown),
    )
    .await?;
    let payload = chatgpt_recipe::ChatgptRecipeOutput {
        transport: "chrome-devtools-mcp".to_string(),
        backend: "chrome-devtools-mcp".to_string(),
        response: response.response,
        model_used: response.model_used,
        warnings: Vec::new(),
        fallback_used: false,
        delivery_mode: chatgpt_recipe::ChatgptDeliveryMode::FileUpload,
        auto_paste_fallback: false,
    }
    .to_value();
    maybe_write_output(ctx, &payload)?;

    match format {
        OutputFormat::Json => {
            write_json(&payload)?;
        }
        OutputFormat::Jsonl => {
            let event = chatgpt_recipe::ChatgptRecipeOutput {
                transport: "chrome-devtools-mcp".to_string(),
                backend: "chrome-devtools-mcp".to_string(),
                response: payload["response"].as_str().unwrap_or_default().to_string(),
                model_used: payload["model_used"].as_str().map(str::to_owned),
                warnings: payload["warnings"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
                fallback_used: false,
                delivery_mode: chatgpt_recipe::ChatgptDeliveryMode::FileUpload,
                auto_paste_fallback: false,
            }
            .to_recipe_complete_event();
            write_jsonl("browser.recipe", &event)?;
        }
        OutputFormat::Text | OutputFormat::Markdown => {
            println!("{}", payload["response"].as_str().unwrap_or_default());
        }
    }

    Ok(payload)
}

fn build_chatgpt_recipe_spec(
    recipe_args: &BrowserRecipeArgs,
    recipe_vars: &BTreeMap<String, String>,
) -> Result<chatgpt_recipe::ChatgptRecipeSpec> {
    let poll_settings = dev_browser::resolve_chatgpt_poll_settings(recipe_vars)?;
    chrome_devtools_mcp::RecipeThreadMode::parse(recipe_vars.get("thread").map(String::as_str))?;
    Ok(chatgpt_recipe::ChatgptRecipeSpec {
        bundle_path: recipe_args.bundle.clone(),
        model: recipe_vars.get("model").cloned().unwrap_or_default(),
        prompt: recipe_vars
            .get("prompt")
            .cloned()
            .unwrap_or_else(|| "Review the attached file and provide your analysis.".to_string()),
        browser_context_id: recipe_vars
            .get("browser_context_id")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        profile_email: recipe_vars
            .get("profile_email")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        run_id: recipe_vars
            .get("run_id")
            .cloned()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(chatgpt_web::generate_run_id),
        wait_timeout_ms: poll_settings.timeout_ms,
        wait_interval_ms: poll_settings.interval_ms,
        disable_extended: recipe_vars
            .get("extended")
            .is_some_and(|value| value == "false"),
    })
}

fn resolve_dev_browser_delivery_mode(
    recipe_args: &BrowserRecipeArgs,
    recipe_vars: &BTreeMap<String, String>,
) -> Result<(bool, Option<String>, bool)> {
    resolve_dev_browser_delivery_mode_for_platform(
        cfg!(target_os = "macos"),
        recipe_args,
        recipe_vars,
    )
}

fn resolve_dev_browser_delivery_mode_for_platform(
    supports_clipboard_file_upload: bool,
    recipe_args: &BrowserRecipeArgs,
    recipe_vars: &BTreeMap<String, String>,
) -> Result<(bool, Option<String>, bool)> {
    let requested_paste_mode = recipe_vars
        .get("paste")
        .is_some_and(|value| value == "true");
    let auto_paste_fallback =
        !requested_paste_mode && recipe_args.bundle.is_some() && !supports_clipboard_file_upload;
    let effective_paste_mode = requested_paste_mode || auto_paste_fallback;
    let bundle_text = if effective_paste_mode {
        recipe_args
            .bundle
            .as_ref()
            .map(fs::read_to_string)
            .transpose()?
    } else {
        None
    };
    Ok((effective_paste_mode, bundle_text, auto_paste_fallback))
}

fn run_recipe_via_dev_browser(
    ctx: &AppContext,
    recipe_args: &BrowserRecipeArgs,
    recipe_vars: &BTreeMap<String, String>,
    cdp_endpoint: Option<&str>,
    format: OutputFormat,
    is_chatgpt: bool,
) -> Result<Value> {
    if !is_chatgpt {
        return Err(anyhow!(
            "dev-browser transport currently supports only the built-in `chatgpt` recipe"
        ));
    }
    chatgpt_web::validate_thread_mode(recipe_vars.get("thread").map(String::as_str))?;
    if recipe_args.profile.is_some() {
        return Err(anyhow!(
            "dev-browser transport does not support `--profile`; use `--cdp` to target a specific Chrome instance/profile"
        ));
    }

    dev_browser::ensure_installed()?;
    // The recipe prepare micro-script already verifies ChatGPT login state on the
    // named page. Avoid a separate pre-flight attach here because it can trigger
    // a fresh approval-gated CDP connection and block an otherwise working flow.

    let (paste_mode, bundle_text, auto_paste_fallback) =
        resolve_dev_browser_delivery_mode(recipe_args, recipe_vars)?;
    if auto_paste_fallback {
        eprintln!(
            "info: dev-browser clipboard file upload requires macOS; falling back to paste mode for this run"
        );
    }
    let recipe_spec = build_chatgpt_recipe_spec(recipe_args, recipe_vars)?;
    let recipe_ctx = dev_browser::DevBrowserRecipeContext {
        bundle_path: recipe_spec.bundle_path.clone(),
        bundle_text,
        model: recipe_spec.model.clone(),
        disable_extended: recipe_spec.disable_extended,
        paste_mode,
        prompt: recipe_spec.prompt.clone(),
        run_id: recipe_spec.run_id.clone(),
        poll_settings: dev_browser::ChatgptPollSettings {
            timeout_ms: recipe_spec.wait_timeout_ms,
            interval_ms: recipe_spec.wait_interval_ms,
        },
        allow_empty_response: recipe_vars
            .get("allow_empty_response")
            .is_some_and(|value| value == "true"),
        cdp_endpoint: cdp_endpoint.map(str::to_owned),
        show_approval_guidance: matches!(format, OutputFormat::Text | OutputFormat::Markdown),
    };

    let response = dev_browser::run_chatgpt_recipe(&recipe_ctx)?;
    let output = chatgpt_recipe::ChatgptRecipeOutput {
        transport: "dev-browser".to_string(),
        backend: "dev-browser".to_string(),
        response: response.response,
        model_used: response.model_used,
        warnings: response.warnings,
        fallback_used: true,
        delivery_mode: if paste_mode {
            chatgpt_recipe::ChatgptDeliveryMode::Paste
        } else {
            chatgpt_recipe::ChatgptDeliveryMode::FileUpload
        },
        auto_paste_fallback,
    };
    let payload = output.to_value();
    maybe_write_output(ctx, &payload)?;
    match format {
        OutputFormat::Json => {
            write_json(&payload)?;
        }
        OutputFormat::Jsonl => {
            let event = output.to_recipe_complete_event();
            write_jsonl("browser.recipe", &event)?;
        }
        OutputFormat::Text | OutputFormat::Markdown => {
            println!("{}", payload["response"].as_str().unwrap_or_default());
        }
    }

    Ok(payload)
}

fn run_recipe_via_agent_browser(
    ctx: &AppContext,
    recipe: browser::Recipe,
    recipe_args: &BrowserRecipeArgs,
    recipe_vars: BTreeMap<String, String>,
    profile_dir: PathBuf,
    format: OutputFormat,
    is_chatgpt: bool,
    prefer_auto_connect: bool,
    selected_cdp_target: &mut Option<browser::ResolvedCdpTarget>,
) -> Result<Value> {
    let needs_auth = is_chatgpt;
    let live_connection = if needs_auth {
        if profile_forces_managed_browser(
            recipe_args.profile.as_deref(),
            recipe_args.cdp.as_deref(),
            recipe_args.browser_id.as_deref(),
        ) {
            None
        } else if let Some(target) = selected_cdp_target.as_ref().cloned() {
            Some(browser::BrowserConnection::Cdp {
                endpoint: target.endpoint,
            })
        } else if prefer_auto_connect {
            // Avoid a separate auto-connect probe here. The recipe run should
            // establish the single running-profile live session we keep.
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

    let needs_bundle_text = recipe.steps.iter().any(|step| {
        step.args
            .as_ref()
            .map(|args| {
                args.iter().any(|arg| {
                    arg.contains("{{bundle_text}}") || arg.contains("{{bundle_text|json}}")
                })
            })
            .unwrap_or(false)
    });
    let bundle_text = match (needs_bundle_text, recipe_args.bundle.as_ref()) {
        (true, Some(path)) => Some(fs::read_to_string(path)?),
        _ => None,
    };

    let recipe_ctx = browser::RecipeContext {
        bundle_path: recipe_args
            .bundle
            .as_ref()
            .map(|path| path.to_string_lossy().to_string()),
        bundle_text,
        profile_dir: Some(profile_dir),
        profile_mode,
        use_stealth: needs_auth,
        headed: needs_auth,
        target_url: browser::CHATGPT_URL.to_string(),
        vars: recipe_vars,
    };

    if let Some(connection) = live_connection {
        let payload =
            browser::run_recipe_with_live_connection(recipe, recipe_ctx, &connection, format)?;
        maybe_write_output(ctx, &payload)?;
        Ok(payload)
    } else {
        let payload = browser::run_recipe(recipe, recipe_ctx, format)?;
        maybe_write_output(ctx, &payload)?;
        Ok(payload)
    }
}

async fn handle_browser(ctx: &AppContext, args: BrowserArgs, format: OutputFormat) -> Result<()> {
    match args.command {
        BrowserCommand::LiveAttachDaemon(_) => live_attach::serve_daemon().await,
        BrowserCommand::Exec(exec) => {
            // dev-browser exec: if args look like a script (single arg with
            // JS-like content or starts with "const"/"await"/"//"), run as script.
            // Otherwise fall back to agent-browser for backward compat.
            if browser::use_dev_browser() {
                let joined = exec.args.join(" ");
                let is_script = exec.args.len() == 1
                    && (joined.contains("await ")
                        || joined.starts_with("const ")
                        || joined.starts_with("//"));
                if is_script {
                    let stdout = dev_browser::run_script_connect(&joined, None)?;
                    print!("{stdout}");
                    return Ok(());
                }
            }
            let stdout = browser::run_agent_browser(exec.args, format, None)?;
            print!("{stdout}");
            Ok(())
        }
        BrowserCommand::Login(login_args) => {
            let profile_dir =
                browser::resolve_profile_dir(&ctx.browser_defaults, login_args.profile.as_ref())?;

            // If --cdp / --browser-id explicitly passed, try CDP first (login is
            // conservative: no auto-discovery unless user explicitly requests it).
            if let Some(explicit_target) = browser::resolve_cdp_target_with_selector(
                login_args.cdp.as_deref(),
                login_args.browser_id.as_deref(),
                &ctx.browser_defaults,
                false,
            )? {
                let cdp_url = explicit_target.endpoint.clone();
                match browser::try_cdp_attach(&cdp_url, "https://chatgpt.com/") {
                    Ok(()) => {
                        let payload = json!({
                            "status": "ok",
                            "method": if login_args.cdp.is_some() {
                                "cdp_explicit".to_string()
                            } else {
                                format!(
                                    "browser_id: {}",
                                    login_args.browser_id.as_deref().unwrap_or_default()
                                )
                            },
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
                    Err(e) => return Err(explicit_cdp_attach_failure(e)),
                }
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
            let managed_profile_only = profile_forces_managed_browser(
                check_args.profile.as_deref(),
                check_args.cdp.as_deref(),
                check_args.browser_id.as_deref(),
            );
            let explicit_browser_target =
                check_args.cdp.is_some() || check_args.browser_id.is_some();
            let mut resolved_cdp_target = browser::resolve_cdp_target_with_selector(
                check_args.cdp.as_deref(),
                check_args.browser_id.as_deref(),
                &ctx.browser_defaults,
                !managed_profile_only,
            )?;
            maybe_print_auto_selected_cdp_target(resolved_cdp_target.as_ref(), format);
            let show_approval_guidance =
                matches!(format, OutputFormat::Text | OutputFormat::Markdown);
            if explicit_browser_target {
                let cdp_url = resolved_cdp_target
                    .as_ref()
                    .map(|target| target.endpoint.as_str())
                    .expect("explicit browser target should resolve");
                live_attach::ensure_chatgpt_session(
                    resolved_cdp_target.as_ref(),
                    None,
                    None,
                    show_approval_guidance,
                )
                .await
                .map_err(explicit_cdp_attach_failure)?;
                maybe_remember_cdp_target(resolved_cdp_target.as_ref(), format);
                let payload = json!({
                    "status": "ok",
                    "method": if check_args.cdp.is_some() {
                        format!("cdp: {cdp_url}")
                    } else {
                        format!(
                            "browser_id: {}",
                            check_args.browser_id.as_deref().unwrap_or_default()
                        )
                    },
                    "transport": "chrome-devtools-mcp",
                });
                return match format {
                    OutputFormat::Json => write_json(&payload),
                    OutputFormat::Jsonl => write_jsonl("browser.check", &payload),
                    OutputFormat::Text | OutputFormat::Markdown => {
                        println!(
                            "Browser authenticated via {} (chrome-devtools-mcp)",
                            if check_args.cdp.is_some() {
                                format!("cdp: {cdp_url}")
                            } else {
                                format!(
                                    "browser_id {}",
                                    check_args.browser_id.as_deref().unwrap_or_default()
                                )
                            }
                        );
                        Ok(())
                    }
                };
            }

            let profile_dir =
                browser::resolve_profile_dir(&ctx.browser_defaults, check_args.profile.as_ref())?;
            let live_attach_owner_is_present =
                live_attach_owner_present(&live_attach::inspect_daemon_sync());
            let prefer_auto_connect = !managed_profile_only
                && should_prefer_running_profile_auto_connect(
                    resolved_cdp_target.as_ref(),
                    live_attach_owner_is_present,
                );
            maybe_print_running_profile_auto_connect_preference(prefer_auto_connect, format);
            let transports = browser_check_transports(
                browser::use_dev_browser(),
                managed_profile_only,
                prefer_auto_connect,
            );
            let mut prior_live_attach_failure: Option<String> = None;
            let mut check_errors: Vec<(BrowserCheckTransport, String)> = Vec::new();

            for transport in transports {
                match transport {
                    BrowserCheckTransport::ChromeDevtoolsMcp => {
                        match live_attach::ensure_chatgpt_session(
                            resolved_cdp_target.as_ref(),
                            None,
                            None,
                            show_approval_guidance,
                        )
                        .await
                        {
                            Ok(_) => {
                                maybe_remember_cdp_target(resolved_cdp_target.as_ref(), format);
                                let method =
                                    browser_check_live_method(resolved_cdp_target.as_ref());
                                let payload = json!({
                                    "status": "ok",
                                    "method": method,
                                    "transport": browser_check_transport_name(transport),
                                });
                                return match format {
                                    OutputFormat::Json => write_json(&payload),
                                    OutputFormat::Jsonl => write_jsonl("browser.check", &payload),
                                    OutputFormat::Text | OutputFormat::Markdown => {
                                        println!(
                                            "Browser authenticated via {} ({})",
                                            payload["method"].as_str().unwrap_or("auto_connect"),
                                            browser_check_transport_name(transport)
                                        );
                                        Ok(())
                                    }
                                };
                            }
                            Err(e) => {
                                if live_attach::is_daemon_rpc_timeout_error(&e) {
                                    return Err(live_attach_daemon_timeout_fallback_error(
                                        "browser check",
                                        e,
                                    ));
                                }
                                if browser::is_chrome_approval_wait_error(&e) {
                                    return Err(e);
                                }
                                remember_browser_check_live_attach_failure(
                                    &mut prior_live_attach_failure,
                                    &e,
                                );
                                if resolved_cdp_target
                                    .as_ref()
                                    .is_some_and(browser::ResolvedCdpTarget::is_authoritative)
                                {
                                    return Err(resolved_cdp_attach_failure(
                                        e,
                                        resolved_cdp_target.as_ref().expect("checked above"),
                                    ));
                                }
                                if resolved_cdp_target
                                    .as_ref()
                                    .is_some_and(browser::ResolvedCdpTarget::is_auto_discovered)
                                {
                                    maybe_demote_auto_selected_cdp_target(
                                        &mut resolved_cdp_target,
                                        format,
                                        &e,
                                    );
                                }
                                eprintln!(
                                    "info: {} auth check failed ({e}), trying next transport",
                                    browser_check_transport_name(transport)
                                );
                                check_errors.push((transport, format!("{e:#}")));
                            }
                        }
                    }
                    BrowserCheckTransport::DevBrowser => {
                        let cdp_endpoint = resolved_cdp_target
                            .as_ref()
                            .map(|target| target.endpoint.as_str());
                        match dev_browser::ensure_chatgpt_auth_with_page_check_and_endpoint(
                            cdp_endpoint,
                        ) {
                            Ok(()) => {
                                let payload = json!({
                                    "status": "ok",
                                    "method": if cdp_endpoint.is_some() {
                                        "cdp"
                                    } else {
                                        "auto_connect"
                                    },
                                    "transport": browser_check_transport_name(transport),
                                });
                                maybe_remember_cdp_target(resolved_cdp_target.as_ref(), format);
                                return match format {
                                    OutputFormat::Json => write_json(&payload),
                                    OutputFormat::Jsonl => write_jsonl("browser.check", &payload),
                                    OutputFormat::Text | OutputFormat::Markdown => {
                                        println!(
                                            "Browser authenticated via {} ({})",
                                            payload["method"].as_str().unwrap_or("auto_connect"),
                                            browser_check_transport_name(transport)
                                        );
                                        Ok(())
                                    }
                                };
                            }
                            Err(e) => {
                                if browser::is_chrome_approval_wait_error(&e) {
                                    return Err(e);
                                }
                                remember_browser_check_live_attach_failure(
                                    &mut prior_live_attach_failure,
                                    &e,
                                );
                                if resolved_cdp_target
                                    .as_ref()
                                    .is_some_and(browser::ResolvedCdpTarget::is_authoritative)
                                {
                                    return Err(resolved_cdp_attach_failure(
                                        e.context("dev-browser auth check failed"),
                                        resolved_cdp_target.as_ref().expect("checked above"),
                                    ));
                                }
                                if resolved_cdp_target
                                    .as_ref()
                                    .is_some_and(browser::ResolvedCdpTarget::is_auto_discovered)
                                {
                                    maybe_demote_auto_selected_cdp_target(
                                        &mut resolved_cdp_target,
                                        format,
                                        &e,
                                    );
                                }
                                eprintln!(
                                    "info: {} auth check failed ({e}), trying next transport",
                                    browser_check_transport_name(transport)
                                );
                                check_errors.push((transport, format!("{e:#}")));
                            }
                        }
                    }
                    BrowserCheckTransport::AgentBrowser => {
                        let connection = if managed_profile_only {
                            browser::resolve_auth(&profile_dir, /* headed */ false)?
                        } else if prefer_auto_connect {
                            browser::try_auto_connect("https://chatgpt.com/").map_err(|e| {
                                if let Some(recovery) = default_daemon_recovery_error(Some(&e)) {
                                    return recovery;
                                }
                                maybe_prefer_browser_check_live_attach_failure(
                                    anyhow!(
                                        "running-profile auto-connect was unavailable ({e}). yoetz will not fall back to a managed profile for this check."
                                    ),
                                    prior_live_attach_failure.as_deref(),
                                )
                            })?;
                            browser::BrowserConnection::AutoConnect
                        } else {
                            let fallback_cdp = resolved_cdp_target
                                .as_ref()
                                .map(|target| target.endpoint.as_str());
                            browser::resolve_browser_connection(
                                &ctx.browser_defaults,
                                fallback_cdp.or(check_args.cdp.as_deref()),
                                &profile_dir,
                                "https://chatgpt.com/",
                            )
                            .map_err(|e| {
                                if let Some(recovery) = default_daemon_recovery_error(Some(&e)) {
                                    return recovery;
                                }
                                maybe_prefer_browser_check_live_attach_failure(
                                    e,
                                    prior_live_attach_failure.as_deref(),
                                )
                            })?
                        };
                        let method = match &connection {
                            browser::BrowserConnection::Cdp { endpoint } => {
                                format!("cdp: {endpoint}")
                            }
                            browser::BrowserConnection::AutoConnect => "auto_connect".to_string(),
                            browser::BrowserConnection::CookieState { .. } => {
                                "cookie_state".to_string()
                            }
                            browser::BrowserConnection::Profile { .. } => "profile".to_string(),
                        };
                        let payload = json!({
                            "status": "ok",
                            "profile": profile_dir.to_string_lossy(),
                            "method": method,
                            "transport": browser_check_transport_name(transport),
                        });
                        if matches!(connection, browser::BrowserConnection::Cdp { .. }) {
                            maybe_remember_cdp_target(resolved_cdp_target.as_ref(), format);
                        }
                        return match format {
                            OutputFormat::Json => write_json(&payload),
                            OutputFormat::Jsonl => write_jsonl("browser.check", &payload),
                            OutputFormat::Text | OutputFormat::Markdown => {
                                println!(
                                    "Browser authenticated via {} ({})",
                                    payload["method"].as_str().unwrap_or("auto_connect"),
                                    browser_check_transport_name(transport)
                                );
                                Ok(())
                            }
                        };
                    }
                }
            }

            Err(browser_check_exhausted_error(
                &check_errors,
                prior_live_attach_failure.as_deref(),
            ))
        }
        BrowserCommand::Doctor(args) => {
            let report = browser::browser_doctor_report(args.live);
            match format {
                OutputFormat::Json => write_json(&json!({ "report": report })),
                OutputFormat::Jsonl => write_jsonl("browser.doctor", &json!({ "report": report })),
                OutputFormat::Text | OutputFormat::Markdown => {
                    println!("{report}");
                    Ok(())
                }
            }
        }
        BrowserCommand::Reset(_) => {
            let dev_browser_stopped = if browser::use_dev_browser() {
                dev_browser::stop_daemon()?
            } else {
                false
            };
            live_attach::reset().await?;
            browser::close_live_attach_session()?;
            browser::close_browser()?;
            let default_daemon_reset = browser::force_kill_stale_daemon();

            let payload = json!({
                "status": "ok",
                "dev_browser_daemon_stopped": dev_browser_stopped,
                "live_attach_state_cleared": true,
                "agent_browser_default": format!("{default_daemon_reset:?}"),
                "agent_browser_cdp_session_closed": true,
            });
            match format {
                OutputFormat::Json => write_json(&payload),
                OutputFormat::Jsonl => write_jsonl("browser.reset", &payload),
                OutputFormat::Text | OutputFormat::Markdown => {
                    if dev_browser_stopped {
                        println!("Stopped dev-browser daemon.");
                    } else if browser::use_dev_browser() {
                        println!("dev-browser daemon was not running.");
                    }
                    println!("Closed agent-browser live-attach session.");
                    println!("Reset agent-browser default daemon state: {default_daemon_reset:?}.");
                    Ok(())
                }
            }
        }
        BrowserCommand::SyncCookies(sync_args) => {
            let profile_dir =
                browser::resolve_profile_dir(&ctx.browser_defaults, sync_args.profile.as_ref())?;
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
            let recipe: browser::Recipe = serde_yaml_ng::from_str(&content)?;
            let mut recipe_vars =
                browser::build_recipe_vars(recipe.defaults.as_ref(), &recipe_args.vars)?;
            let profile_dir =
                browser::resolve_profile_dir(&ctx.browser_defaults, recipe_args.profile.as_ref())?;
            let is_chatgpt = is_chatgpt_recipe(&recipe, &recipe_path);
            let managed_profile_only = profile_forces_managed_browser(
                recipe_args.profile.as_deref(),
                recipe_args.cdp.as_deref(),
                recipe_args.browser_id.as_deref(),
            );
            if is_chatgpt {
                chatgpt_web::validate_thread_mode(recipe_vars.get("thread").map(String::as_str))?;
                recipe_vars
                    .entry("run_id".to_string())
                    .or_insert_with(chatgpt_web::generate_run_id);
            }
            let mut resolved_cdp_target = browser::resolve_cdp_target_with_selector(
                recipe_args.cdp.as_deref(),
                recipe_args.browser_id.as_deref(),
                &ctx.browser_defaults,
                !managed_profile_only,
            )?;
            maybe_print_auto_selected_cdp_target(resolved_cdp_target.as_ref(), format);
            let transports = constrain_chatgpt_transports_for_browser_context_selector(
                browser::recipe_transports(&recipe, is_chatgpt),
                &recipe_vars,
                is_chatgpt,
            );
            let live_attach_owner_is_present =
                live_attach_owner_present(&live_attach::inspect_daemon_sync());
            let prefer_auto_connect = is_chatgpt
                && !managed_profile_only
                && !recipe_uses_exact_browser_context_selector(&recipe_vars)
                && should_prefer_running_profile_auto_connect(
                    resolved_cdp_target.as_ref(),
                    live_attach_owner_is_present,
                );
            maybe_print_running_profile_auto_connect_preference(prefer_auto_connect, format);
            let transports = prioritize_chatgpt_transports_for_running_profile_auto_connect(
                transports,
                prefer_auto_connect,
            );
            let manual_fallback =
                manual_browser_recipe_fallback(&recipe_path, recipe_args.bundle.as_deref());
            let mut transport_errors = Vec::new();

            let mut skip_remaining_live_cdp = false;
            for (index, transport) in transports.iter().copied().enumerate() {
                let cdp_endpoint = resolved_cdp_target
                    .as_ref()
                    .map(|target| target.endpoint.clone());
                if skip_remaining_live_cdp && is_live_cdp_only_transport(transport) {
                    eprintln!(
                        "info: skipping {} transport — Chrome CDP was unreachable in an earlier tier",
                        recipe_transport_name(transport)
                    );
                    continue;
                }
                if !matches!(transport, browser::RecipeTransport::Manual) {
                    eprintln!(
                        "info: attempting {} transport",
                        recipe_transport_name(transport)
                    );
                }

                let result = match transport {
                    browser::RecipeTransport::DevBrowser => run_recipe_via_dev_browser(
                        ctx,
                        &recipe_args,
                        &recipe_vars,
                        cdp_endpoint.as_deref(),
                        format,
                        is_chatgpt,
                    ),
                    browser::RecipeTransport::AgentBrowser => run_recipe_via_agent_browser(
                        ctx,
                        recipe.clone(),
                        &recipe_args,
                        recipe_vars.clone(),
                        profile_dir.clone(),
                        format,
                        is_chatgpt,
                        prefer_auto_connect,
                        &mut resolved_cdp_target,
                    ),
                    browser::RecipeTransport::ChromeDevtoolsMcp => {
                        run_recipe_via_chrome_devtools_mcp(
                            ctx,
                            &recipe_args,
                            &recipe_vars,
                            resolved_cdp_target.as_ref(),
                            format,
                            is_chatgpt,
                        )
                        .await
                    }
                    browser::RecipeTransport::Manual => Err(anyhow!("{}", manual_fallback)),
                };

                match result {
                    Ok(_payload) => {
                        if matches!(
                            transport,
                            browser::RecipeTransport::ChromeDevtoolsMcp
                                | browser::RecipeTransport::DevBrowser
                                | browser::RecipeTransport::AgentBrowser
                        ) {
                            maybe_remember_cdp_target(resolved_cdp_target.as_ref(), format);
                        }
                        return Ok(());
                    }
                    Err(err) => {
                        if resolved_cdp_target
                            .as_ref()
                            .is_some_and(browser::ResolvedCdpTarget::is_auto_discovered)
                        {
                            maybe_demote_auto_selected_cdp_target(
                                &mut resolved_cdp_target,
                                format,
                                &err,
                            );
                        }
                        if recipe_should_stop_live_transport_fallback(
                            &err,
                            resolved_cdp_target.as_ref(),
                            transport,
                            &recipe_vars,
                        ) {
                            transport_errors.push((transport, recipe_transport_error_detail(&err)));
                            if recipe_has_remaining_manual_fallback(&transports, index) {
                                transport_errors.push((
                                    browser::RecipeTransport::Manual,
                                    manual_fallback.clone(),
                                ));
                            }
                            break;
                        }
                        if recipe_should_skip_remaining_live_cdp_transports(&err) {
                            skip_remaining_live_cdp = true;
                        }
                        if !matches!(transport, browser::RecipeTransport::Manual) {
                            eprintln!(
                                "info: {} transport failed ({err}), trying next transport",
                                recipe_transport_name(transport)
                            );
                        }
                        transport_errors.push((transport, recipe_transport_error_detail(&err)));
                    }
                }
            }

            Err(anyhow!(format_recipe_transport_errors(&transport_errors)))
        }
        BrowserCommand::Attach(attach_args) => {
            // Try explicit CDP first, then auto-connect. No cookie fallback for attach.
            let explicit_browser_target =
                attach_args.cdp.is_some() || attach_args.browser_id.is_some();
            let mut resolved_cdp_target = browser::resolve_cdp_target_with_selector(
                attach_args.cdp.as_deref(),
                attach_args.browser_id.as_deref(),
                &ctx.browser_defaults,
                true,
            )?;
            maybe_print_auto_selected_cdp_target(resolved_cdp_target.as_ref(), format);
            let cdp_endpoint = resolved_cdp_target
                .as_ref()
                .map(|target| target.endpoint.clone());
            let show_approval_guidance =
                matches!(format, OutputFormat::Text | OutputFormat::Markdown);
            let live_attach_owner_is_present =
                live_attach_owner_present(&live_attach::inspect_daemon_sync());
            let prefer_auto_connect = should_prefer_running_profile_auto_connect(
                resolved_cdp_target.as_ref(),
                live_attach_owner_is_present,
            );
            maybe_print_running_profile_auto_connect_preference(prefer_auto_connect, format);
            if resolved_cdp_target.is_some() {
                match live_attach::ensure_chatgpt_session(
                    resolved_cdp_target.as_ref(),
                    None,
                    None,
                    show_approval_guidance,
                )
                .await
                {
                    Ok(_) => {
                        maybe_remember_cdp_target(resolved_cdp_target.as_ref(), format);
                        let method = if attach_args.cdp.is_some() {
                            "cdp_explicit".to_string()
                        } else if attach_args.browser_id.is_some() {
                            format!(
                                "browser_id: {}",
                                attach_args.browser_id.as_deref().unwrap_or_default()
                            )
                        } else {
                            "cdp_selected".to_string()
                        };
                        let payload = json!({
                            "status": "ok",
                            "method": method,
                            "endpoint": cdp_endpoint.as_deref(),
                            "transport": "chrome-devtools-mcp",
                        });
                        return match format {
                            OutputFormat::Json => write_json(&payload),
                            OutputFormat::Jsonl => write_jsonl("browser.attach", &payload),
                            OutputFormat::Text | OutputFormat::Markdown => {
                                let endpoint = cdp_endpoint
                                    .as_deref()
                                    .expect("resolved cdp target should have an endpoint");
                                println!(
                                    "Attached via {}",
                                    if attach_args.cdp.is_some() {
                                        format!("CDP: {endpoint}")
                                    } else if attach_args.browser_id.is_some() {
                                        format!(
                                            "browser_id {} ({endpoint})",
                                            attach_args.browser_id.as_deref().unwrap_or_default()
                                        )
                                    } else {
                                        format!("CDP: {endpoint}")
                                    }
                                );
                                Ok(())
                            }
                        };
                    }
                    Err(e) if live_attach::is_daemon_rpc_timeout_error(&e) => {
                        return Err(live_attach_daemon_timeout_fallback_error(
                            "browser attach",
                            e,
                        ));
                    }
                    Err(e) if explicit_browser_target => {
                        if let Some(target) = resolved_cdp_target.as_ref() {
                            return Err(resolved_cdp_attach_failure(e, target));
                        }
                        return Err(explicit_cdp_attach_failure(e));
                    }
                    Err(e)
                        if resolved_cdp_target
                            .as_ref()
                            .is_some_and(browser::ResolvedCdpTarget::is_authoritative) =>
                    {
                        let target = resolved_cdp_target.as_ref().expect("checked above");
                        return Err(resolved_cdp_attach_failure(e, target));
                    }
                    Err(e)
                        if resolved_cdp_target
                            .as_ref()
                            .is_some_and(browser::ResolvedCdpTarget::is_auto_discovered) =>
                    {
                        maybe_demote_auto_selected_cdp_target(&mut resolved_cdp_target, format, &e);
                    }
                    Err(_) => {}
                }
            }

            if resolved_cdp_target.is_none() && !prefer_auto_connect {
                match live_attach::ensure_chatgpt_session(None, None, None, show_approval_guidance)
                    .await
                {
                    Ok(_) => {
                        let payload = json!({
                            "status": "ok",
                            "method": "cdp_auto",
                            "transport": "chrome-devtools-mcp",
                        });
                        return match format {
                            OutputFormat::Json => write_json(&payload),
                            OutputFormat::Jsonl => write_jsonl("browser.attach", &payload),
                            OutputFormat::Text | OutputFormat::Markdown => {
                                println!("Attached via chrome-devtools-mcp");
                                Ok(())
                            }
                        };
                    }
                    Err(err) if live_attach::is_daemon_rpc_timeout_error(&err) => {
                        return Err(live_attach_daemon_timeout_fallback_error(
                            "browser attach",
                            err,
                        ));
                    }
                    Err(err) if explicit_browser_target => {
                        return Err(explicit_cdp_attach_failure(err))
                    }
                    Err(err) => {
                        if browser::is_chrome_approval_wait_error(&err)
                            || browser::is_chatgpt_auth_issue_error(&err)
                        {
                            return Err(err);
                        }
                        if matches!(format, OutputFormat::Text | OutputFormat::Markdown) {
                            eprintln!(
                                "info: live-attach owner setup failed ({err}); falling back to running-profile auto-connect"
                            );
                        }
                    }
                }
            }

            match browser::try_auto_connect("https://chatgpt.com/") {
                Ok(()) => {
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
                Err(err) => {
                    if browser::is_chrome_approval_wait_error(&err)
                        || browser::is_chatgpt_auth_issue_error(&err)
                    {
                        return Err(err);
                    }
                    if let Some(recovery) = default_daemon_recovery_error(Some(&err)) {
                        return Err(recovery);
                    }
                }
            }

            Err(anyhow!(
                "could not attach to any Chrome instance.\n\n\
                 Recommended: enable remote debugging at chrome://inspect/#remote-debugging (Chrome 144+)\n\
                 Alternative: pass --cdp <url> with Chrome launched using --user-data-dir\n\n\
                 Note: since Chrome 136, --remote-debugging-port is ignored on the default profile.\n\
                 See: https://developer.chrome.com/blog/remote-debugging-port"
            ))
        }
        BrowserCommand::VerifyCdp(args) => {
            // Thin CDP smoke: attach and open an `about:blank` tab. Used by
            // the real-browser CI lane (review finding #13) against a fresh
            // Chrome for Testing instance so the deeper `check` / `attach`
            // auth-probe path is not exercised.
            let client = chrome_devtools_mcp::client::CdpMcpClient::connect_to_running_chrome(
                Some(&args.cdp),
            )
            .await
            .with_context(|| format!("attaching to CDP endpoint `{}`", args.cdp))?;
            let new_page = client
                .new_page(&args.url, /* background */ true, 15_000, None)
                .await
                .with_context(|| format!("opening `{}` against `{}`", args.url, args.cdp))?;
            let payload = json!({
                "status": "ok",
                "endpoint": args.cdp,
                "url": args.url,
                "page_id": new_page.page_id,
            });
            let _ = client.close_selected_page(true);
            match format {
                OutputFormat::Json => write_json(&payload),
                OutputFormat::Jsonl => write_jsonl("browser.verify_cdp", &payload),
                OutputFormat::Text | OutputFormat::Markdown => {
                    println!(
                        "verify-cdp ok: endpoint={} page={}",
                        args.cdp, new_page.page_id
                    );
                    Ok(())
                }
            }
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
    let fence = markdown_fence(diff);
    prompt.push_str(&format!("\nDiff:\n{fence}diff\n"));
    prompt.push_str(diff);
    if !diff.ends_with('\n') {
        prompt.push('\n');
    }
    prompt.push_str(&format!("{fence}\n"));
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
    let fence = markdown_fence(content);
    prompt.push_str(&format!("{fence}text\n"));
    prompt.push_str(content);
    if !content.ends_with('\n') {
        prompt.push('\n');
    }
    if truncated {
        prompt.push_str("\n... [truncated]\n");
    }
    prompt.push_str(&format!("{fence}\n"));
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
    for (value, mime) in values.iter().zip(overrides) {
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
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn normalize_model_name(model: &str) -> String {
        normalize_model_name_with_aliases(model, &std::collections::HashMap::new())
    }

    fn test_app_context() -> AppContext {
        let config = Config::default();
        let client = build_client(1).expect("build reqwest client");
        let litellm = Arc::new(build_litellm(&config, client.clone()).expect("build litellm"));
        AppContext {
            config,
            browser_defaults: browser::BrowserDefaults::default(),
            client,
            litellm,
            output_final: None,
            output_schema: None,
            debug: false,
            allow_unknown: false,
        }
    }

    fn temp_schema_path() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("yoetz_schema_{nanos}.json"))
    }

    fn temp_output_path(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}_{nanos}.json"))
    }

    #[test]
    fn review_diff_prompt_uses_safe_fence_length() {
        let diff = "@@ -1 +1 @@\n-```old\n+```new\n";
        let prompt = build_review_diff_prompt(diff, None);

        assert!(prompt.contains("\nDiff:\n````diff\n"));
        assert!(prompt.ends_with("````\n"));
    }

    #[test]
    fn review_file_prompt_uses_safe_fence_length() {
        let prompt = build_review_file_prompt(
            std::path::Path::new("src/lib.rs"),
            "fn main() {\n    println!(\"```\");\n}",
            false,
            None,
        );

        assert!(prompt.contains("\nFile: src/lib.rs\n````text\n"));
        assert!(prompt.ends_with("````\n"));
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
    fn maybe_write_output_writes_output_final_json() {
        let output_path = temp_output_path("yoetz_browser_recipe_output");
        let mut ctx = test_app_context();
        ctx.output_final = Some(output_path.clone());

        let payload = json!({
            "status": "ok",
            "backend": "dev-browser",
            "response": "review text",
        });

        maybe_write_output(&ctx, &payload).unwrap();

        let written: Value =
            serde_json::from_str(&fs::read_to_string(&output_path).unwrap()).unwrap();
        assert_eq!(written, payload);

        let _ = fs::remove_file(output_path);
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
    fn protected_dotenv_env_vars_cover_sensitive_paths_and_targets() {
        for key in [
            "YOETZ_CONFIG_PATH",
            "YOETZ_REGISTRY_PATH",
            "YOETZ_BROWSER_CDP",
            "YOETZ_BROWSER_TARGET_PATH",
            "YOETZ_BROWSER_PROFILE",
            "LITELLM_API_KEY",
        ] {
            assert!(
                BASE_PROTECTED_DOTENV_ENV_VARS.contains(&key),
                "{key} must stay protected"
            );
        }
    }

    #[test]
    fn protected_dotenv_env_vars_include_custom_provider_api_key_envs() {
        let mut config = Config::default();
        config.providers.insert(
            "corp".to_string(),
            yoetz_core::config::ProviderConfig {
                api_key_env: Some("CORP_LLM_TOKEN".to_string()),
                ..Default::default()
            },
        );

        let protected = protected_dotenv_env_vars(&config);

        assert!(protected.iter().any(|key| key == "CORP_LLM_TOKEN"));
        assert!(protected.iter().any(|key| key == "LITELLM_API_KEY"));
    }

    #[test]
    fn recipe_should_stop_live_transport_fallback_on_approval_wait() {
        let err = anyhow!(
            "live browser attach timed out (30s). Chrome may be showing an \"Allow remote debugging?\" dialog — please click Allow in Chrome, then retry."
        );
        let vars = std::collections::BTreeMap::new();
        assert!(recipe_should_stop_live_transport_fallback(
            &err,
            None,
            browser::RecipeTransport::ChromeDevtoolsMcp,
            &vars,
        ));
    }

    #[test]
    fn recipe_should_stop_live_transport_fallback_on_chatgpt_page_errors() {
        let model_mismatch = anyhow!(
            "requested ChatGPT model `pro` was not actually selected. Current page: url=https://chatgpt.com/, title=\"ChatGPT\""
        );
        let auth_issue = anyhow!(
            "chatgpt login required in the attached Chrome session. Log in there and try again."
        );
        let vars = std::collections::BTreeMap::new();
        assert!(!recipe_should_stop_live_transport_fallback(
            &model_mismatch,
            None,
            browser::RecipeTransport::ChromeDevtoolsMcp,
            &vars,
        ));
        assert!(recipe_should_stop_live_transport_fallback(
            &model_mismatch,
            None,
            browser::RecipeTransport::AgentBrowser,
            &vars,
        ));
        assert!(!recipe_should_stop_live_transport_fallback(
            &auth_issue,
            None,
            browser::RecipeTransport::ChromeDevtoolsMcp,
            &vars,
        ));
    }

    #[test]
    fn recipe_should_stop_live_transport_fallback_on_live_attach_daemon_timeout() {
        let err = anyhow!(
            "yoetz live-attach daemon at 127.0.0.1:45555 timed out after 75000ms waiting for a response"
        );
        let vars = std::collections::BTreeMap::new();
        assert!(recipe_should_stop_live_transport_fallback(
            &err,
            None,
            browser::RecipeTransport::ChromeDevtoolsMcp,
            &vars,
        ));
    }

    #[test]
    fn recipe_should_not_stop_live_transport_fallback_on_non_approval_error() {
        let err = anyhow!("chatgpt send button not found");
        let vars = std::collections::BTreeMap::new();
        assert!(!recipe_should_stop_live_transport_fallback(
            &err,
            None,
            browser::RecipeTransport::ChromeDevtoolsMcp,
            &vars,
        ));
    }

    #[test]
    fn recipe_should_stop_live_transport_fallback_when_target_is_selected() {
        let err = anyhow!("chrome-devtools-mcp new_page on chatgpt.com");
        let target = browser::resolve_cdp_target(
            Some("ws://127.0.0.1:9222/devtools/browser/example"),
            &browser::BrowserDefaults::default(),
        )
        .unwrap()
        .unwrap();
        let vars = std::collections::BTreeMap::new();
        assert!(recipe_should_stop_live_transport_fallback(
            &err,
            Some(&target),
            browser::RecipeTransport::ChromeDevtoolsMcp,
            &vars,
        ));
    }

    #[test]
    fn recipe_should_stop_auth_issue_for_authoritative_target() {
        let err = anyhow!(
            "chatgpt login required in the attached Chrome session. Log in there and try again."
        );
        let target = browser::resolve_cdp_target(
            Some("ws://127.0.0.1:9222/devtools/browser/example"),
            &browser::BrowserDefaults::default(),
        )
        .unwrap()
        .unwrap();
        let vars = std::collections::BTreeMap::new();
        assert!(recipe_should_stop_live_transport_fallback(
            &err,
            Some(&target),
            browser::RecipeTransport::ChromeDevtoolsMcp,
            &vars,
        ));
    }

    #[test]
    fn recipe_should_stop_attached_page_errors_for_authoritative_target_or_exact_context() {
        let err = anyhow!(
            "requested ChatGPT model `pro` was not actually selected. Current page: url=https://chatgpt.com/, title=\"ChatGPT\""
        );
        let target = browser::resolve_cdp_target(
            Some("ws://127.0.0.1:9222/devtools/browser/example"),
            &browser::BrowserDefaults::default(),
        )
        .unwrap()
        .unwrap();
        let exact_context_vars = std::collections::BTreeMap::from([(
            "browser_context_id".to_string(),
            "ctx-123".to_string(),
        )]);
        assert!(recipe_should_stop_live_transport_fallback(
            &err,
            Some(&target),
            browser::RecipeTransport::ChromeDevtoolsMcp,
            &std::collections::BTreeMap::new(),
        ));
        assert!(recipe_should_stop_live_transport_fallback(
            &err,
            None,
            browser::RecipeTransport::ChromeDevtoolsMcp,
            &exact_context_vars,
        ));
    }

    #[test]
    fn recipe_should_not_stop_dev_browser_page_errors_before_agent_browser() {
        let err = anyhow!(
            "{}",
            r#"ChatGPT send button never became enabled after typing. {"send":"missing"}"#
        );
        let vars = std::collections::BTreeMap::new();
        assert!(!recipe_should_stop_live_transport_fallback(
            &err,
            None,
            browser::RecipeTransport::DevBrowser,
            &vars,
        ));
    }

    #[test]
    fn recipe_should_skip_remaining_live_cdp_transports_on_cdp_unreachable() {
        // When tier 1 (chrome-devtools-mcp) fails because Chrome is not
        // listening on CDP at all, dev-browser will fail for the same
        // reason and Playwright's `connectOverCDP` hangs on
        // `Target.setAutoAttach` instead of failing fast. Skip remaining
        // pure live-CDP tiers — but NOT agent-browser, which transparently
        // falls back from live-attach to a managed profile with stored
        // cookies and still works without CDP.
        let err =
            anyhow!("requesting `http://127.0.0.1:9222/json/version` failed: connection refused")
                .context(
                    "chrome-devtools-mcp could not reach Chrome's CDP endpoint. \
             Chrome 136+ ignores --remote-debugging-port on the default profile — \
             either enable chrome://inspect/#remote-debugging (Chrome 144+) and retry, \
             or pass --cdp=ws://127.0.0.1:PORT after launching Chrome with a non-default \
             --user-data-dir, or use Chrome for Testing",
                );
        assert!(recipe_should_skip_remaining_live_cdp_transports(&err));
        // Crucial invariant: CDP-unreachable must NOT stop the whole
        // funnel — agent-browser still gets a chance via managed profile.
        assert!(!recipe_should_stop_live_transport_fallback(
            &err,
            None,
            browser::RecipeTransport::ChromeDevtoolsMcp,
            &std::collections::BTreeMap::new(),
        ));
    }

    #[test]
    fn live_attach_daemon_timeout_fallback_error_mentions_no_fallback() {
        let err = live_attach_daemon_timeout_fallback_error(
            "browser check",
            anyhow!(
                "yoetz live-attach daemon at 127.0.0.1:45555 timed out after 75000ms waiting for a response"
            ),
        );
        let message = format!("{err:#}");
        assert!(message.contains("browser check"));
        assert!(message.contains("instead of falling through"));
        assert!(message.contains("yoetz browser reset"));
    }

    #[test]
    fn is_live_cdp_only_transport_excludes_agent_browser_and_manual() {
        assert!(is_live_cdp_only_transport(
            browser::RecipeTransport::ChromeDevtoolsMcp
        ));
        assert!(is_live_cdp_only_transport(
            browser::RecipeTransport::DevBrowser
        ));
        // agent-browser has a managed-profile fallback that does not need
        // a live CDP endpoint, so CDP-unreachable must not skip it.
        assert!(!is_live_cdp_only_transport(
            browser::RecipeTransport::AgentBrowser
        ));
        assert!(!is_live_cdp_only_transport(
            browser::RecipeTransport::Manual
        ));
    }

    #[test]
    fn explicit_cdp_attach_failure_rewrites_approval_waits() {
        let err = anyhow!(
            "live browser attach timed out (30s). Chrome may be showing an \"Allow remote debugging?\" dialog — please click Allow in Chrome, then retry."
        );
        let rewritten = explicit_cdp_attach_failure(err);
        assert!(rewritten.to_string().contains("Allow remote debugging"));
        assert!(!rewritten.to_string().contains("not falling back"));
    }

    #[test]
    fn explicit_cdp_attach_failure_preserves_non_approval_context() {
        let err = anyhow!("browserType.connectOverCDP: failed to list pages");
        let rewritten = explicit_cdp_attach_failure(err);
        let msg = format!("{rewritten:#}");
        assert!(msg.contains("explicit --cdp failed; not falling back"));
        assert!(msg.contains("failed to list pages"));
    }

    #[test]
    fn recipe_has_remaining_manual_fallback_detects_manual_transport() {
        let transports = vec![
            browser::RecipeTransport::DevBrowser,
            browser::RecipeTransport::AgentBrowser,
            browser::RecipeTransport::ChromeDevtoolsMcp,
            browser::RecipeTransport::Manual,
        ];
        assert!(recipe_has_remaining_manual_fallback(&transports, 0));
        assert!(recipe_has_remaining_manual_fallback(&transports, 1));
        assert!(recipe_has_remaining_manual_fallback(&transports, 2));
        assert!(!recipe_has_remaining_manual_fallback(&transports, 3));
    }

    #[test]
    fn recipe_transport_name_covers_chrome_devtools_mcp() {
        assert_eq!(
            recipe_transport_name(browser::RecipeTransport::ChromeDevtoolsMcp),
            "chrome-devtools-mcp"
        );
    }

    #[test]
    fn recipe_uses_chatgpt_browser_context_selector_detects_email_and_context_id() {
        let mut vars = std::collections::BTreeMap::new();
        assert!(!recipe_uses_chatgpt_browser_context_selector(&vars));
        assert!(!recipe_uses_profile_email_selector(&vars));
        assert!(!recipe_uses_exact_browser_context_selector(&vars));

        vars.insert(
            "profile_email".to_string(),
            "avivsinai@gmail.com".to_string(),
        );
        assert!(recipe_uses_chatgpt_browser_context_selector(&vars));
        assert!(recipe_uses_profile_email_selector(&vars));
        assert!(!recipe_uses_exact_browser_context_selector(&vars));

        vars.clear();
        vars.insert("browser_context_id".to_string(), "ctx-123".to_string());
        assert!(recipe_uses_chatgpt_browser_context_selector(&vars));
        assert!(!recipe_uses_profile_email_selector(&vars));
        assert!(recipe_uses_exact_browser_context_selector(&vars));
    }

    #[test]
    fn constrain_chatgpt_transports_for_profile_email_keeps_agent_browser_available() {
        let mut vars = std::collections::BTreeMap::new();
        vars.insert(
            "profile_email".to_string(),
            "avivsinai@gmail.com".to_string(),
        );
        let transports = constrain_chatgpt_transports_for_browser_context_selector(
            vec![
                browser::RecipeTransport::ChromeDevtoolsMcp,
                browser::RecipeTransport::DevBrowser,
                browser::RecipeTransport::AgentBrowser,
                browser::RecipeTransport::Manual,
            ],
            &vars,
            true,
        );
        assert_eq!(
            transports,
            vec![
                browser::RecipeTransport::ChromeDevtoolsMcp,
                browser::RecipeTransport::AgentBrowser,
                browser::RecipeTransport::Manual
            ]
        );
    }

    #[test]
    fn constrain_chatgpt_transports_for_exact_browser_context_id_keeps_only_mcp_and_manual() {
        let vars = std::collections::BTreeMap::from([(
            "browser_context_id".to_string(),
            "ctx-123".to_string(),
        )]);
        let transports = constrain_chatgpt_transports_for_browser_context_selector(
            vec![
                browser::RecipeTransport::ChromeDevtoolsMcp,
                browser::RecipeTransport::DevBrowser,
                browser::RecipeTransport::AgentBrowser,
                browser::RecipeTransport::Manual,
            ],
            &vars,
            true,
        );
        assert_eq!(
            transports,
            vec![
                browser::RecipeTransport::ChromeDevtoolsMcp,
                browser::RecipeTransport::Manual
            ]
        );
    }

    #[test]
    fn recipe_should_not_stop_profile_email_fallback_on_advisory_live_target_visibility_errors() {
        let err = anyhow!(
            "profile_email `aviv.s@taboola.com` did not match any live Chrome browser context"
        );
        let vars = std::collections::BTreeMap::from([(
            "profile_email".to_string(),
            "aviv.s@taboola.com".to_string(),
        )]);
        assert!(!recipe_should_stop_live_transport_fallback(
            &err,
            None,
            browser::RecipeTransport::ChromeDevtoolsMcp,
            &vars,
        ));
    }

    #[test]
    fn prefer_running_profile_auto_connect_requires_implicit_target_and_no_live_owner() {
        assert!(should_prefer_running_profile_auto_connect(None, false));
        assert!(!should_prefer_running_profile_auto_connect(None, true));

        let browser_defaults = browser::BrowserDefaults {
            cdp: Some("ws://127.0.0.1:9222/devtools/browser/config".into()),
            ..Default::default()
        };
        let configured = browser::resolve_cdp_target(None, &browser_defaults)
            .unwrap()
            .expect("configured target");
        assert!(!should_prefer_running_profile_auto_connect(
            Some(&configured),
            false,
        ));
    }

    #[test]
    fn live_attach_owner_present_requires_attached_session_or_busy_daemon() {
        assert!(!live_attach_owner_present(&live_attach::DaemonSummary {
            health: live_attach::DaemonHealth::Healthy,
            pid: Some(1),
            session_count: 0,
        }));
        assert!(live_attach_owner_present(&live_attach::DaemonSummary {
            health: live_attach::DaemonHealth::Healthy,
            pid: Some(1),
            session_count: 1,
        }));
        assert!(live_attach_owner_present(&live_attach::DaemonSummary {
            health: live_attach::DaemonHealth::Busy,
            pid: Some(1),
            session_count: 0,
        }));
        assert!(!live_attach_owner_present(&live_attach::DaemonSummary {
            health: live_attach::DaemonHealth::NotRunning,
            pid: None,
            session_count: 0,
        }));
    }

    #[test]
    fn prioritize_chatgpt_transports_for_running_profile_prefers_mcp_then_dev_browser_and_drops_agent_browser(
    ) {
        let transports = prioritize_chatgpt_transports_for_running_profile_auto_connect(
            vec![
                browser::RecipeTransport::ChromeDevtoolsMcp,
                browser::RecipeTransport::DevBrowser,
                browser::RecipeTransport::AgentBrowser,
                browser::RecipeTransport::Manual,
            ],
            true,
        );
        assert_eq!(
            transports,
            vec![
                browser::RecipeTransport::ChromeDevtoolsMcp,
                browser::RecipeTransport::DevBrowser,
                browser::RecipeTransport::Manual
            ]
        );
    }

    #[test]
    fn prioritize_chatgpt_transports_for_running_profile_keeps_mcp_before_dev_browser_when_agent_browser_is_unavailable(
    ) {
        let transports = prioritize_chatgpt_transports_for_running_profile_auto_connect(
            vec![
                browser::RecipeTransport::ChromeDevtoolsMcp,
                browser::RecipeTransport::DevBrowser,
                browser::RecipeTransport::Manual,
            ],
            true,
        );
        assert_eq!(
            transports,
            vec![
                browser::RecipeTransport::ChromeDevtoolsMcp,
                browser::RecipeTransport::DevBrowser,
                browser::RecipeTransport::Manual
            ]
        );
    }

    #[test]
    fn prioritize_chatgpt_transports_for_running_profile_preserves_explicit_single_transport() {
        let transports = prioritize_chatgpt_transports_for_running_profile_auto_connect(
            vec![browser::RecipeTransport::ChromeDevtoolsMcp],
            true,
        );
        assert_eq!(
            transports,
            vec![browser::RecipeTransport::ChromeDevtoolsMcp]
        );
    }

    #[test]
    fn prioritize_chatgpt_transports_for_running_profile_preserves_cdp_only_manual_fallback() {
        let transports = prioritize_chatgpt_transports_for_running_profile_auto_connect(
            vec![
                browser::RecipeTransport::ChromeDevtoolsMcp,
                browser::RecipeTransport::Manual,
            ],
            true,
        );
        assert_eq!(
            transports,
            vec![
                browser::RecipeTransport::ChromeDevtoolsMcp,
                browser::RecipeTransport::Manual
            ]
        );
    }

    #[test]
    fn browser_check_orders_transports_by_mode() {
        assert_eq!(
            browser_check_transports(false, false, false),
            vec![
                BrowserCheckTransport::ChromeDevtoolsMcp,
                BrowserCheckTransport::AgentBrowser,
            ]
        );
        assert_eq!(
            browser_check_transports(true, false, false),
            vec![
                BrowserCheckTransport::ChromeDevtoolsMcp,
                BrowserCheckTransport::DevBrowser,
                BrowserCheckTransport::AgentBrowser,
            ]
        );
        assert_eq!(
            browser_check_transports(true, true, false),
            vec![BrowserCheckTransport::AgentBrowser]
        );
        assert_eq!(
            browser_check_transports(true, false, true),
            vec![
                BrowserCheckTransport::ChromeDevtoolsMcp,
                BrowserCheckTransport::DevBrowser
            ]
        );
        assert_eq!(
            browser_check_transports(false, false, true),
            vec![BrowserCheckTransport::ChromeDevtoolsMcp]
        );
    }

    #[test]
    fn browser_check_live_method_uses_auto_connect_for_implicit_targets() {
        assert_eq!(browser_check_live_method(None), "auto_connect");

        let browser_defaults = browser::BrowserDefaults {
            cdp: Some("ws://127.0.0.1:9222/devtools/browser/config".into()),
            ..Default::default()
        };
        let configured = browser::resolve_cdp_target(None, &browser_defaults)
            .unwrap()
            .expect("configured target");
        assert_eq!(
            browser_check_live_method(Some(&configured)),
            "cdp: ws://127.0.0.1:9222/devtools/browser/config"
        );
    }

    #[test]
    fn browser_check_prefers_live_attach_failure_over_managed_login_error() {
        let err = maybe_prefer_browser_check_live_attach_failure(
            anyhow!("chatgpt login required. Run `yoetz browser login` and try again."),
            Some("dev-browser could not connect to Chrome. Enable remote debugging: chrome://inspect/#remote-debugging"),
        );
        let message = err.to_string();
        assert!(message.contains("live Chrome attach failed"));
        assert!(message.contains("chrome://inspect/#remote-debugging"));
        assert!(message.contains("chatgpt login required"));
    }

    #[test]
    fn browser_check_keeps_managed_error_without_prior_live_attach_failure() {
        let err = maybe_prefer_browser_check_live_attach_failure(
            anyhow!("chatgpt login required. Run `yoetz browser login` and try again."),
            None,
        );
        assert_eq!(
            err.to_string(),
            "chatgpt login required. Run `yoetz browser login` and try again."
        );
    }

    #[test]
    fn browser_check_exhaustion_reports_dev_browser_failure() {
        let errors = vec![(
            BrowserCheckTransport::DevBrowser,
            "dev-browser connection check failed: Target.setAutoAttach connection closed"
                .to_string(),
        )];
        let err = browser_check_exhausted_error(
            &errors,
            Some("dev-browser could not connect to Chrome before managed fallback"),
        );
        let message = format!("{err:#}");
        assert!(message.contains("browser check failed"));
        assert!(message.contains("dev-browser"));
        assert!(message.contains("Target.setAutoAttach connection closed"));
        assert!(message.contains("dev-browser could not connect to Chrome"));
    }

    #[test]
    fn auto_selected_cdp_target_is_demoted_for_chatgpt_ui_auth_issues() {
        let login_err = anyhow!(
            "chatgpt login required in the attached Chrome session. Log in there and try again."
        );
        let challenge_err = anyhow!(
            "cloudflare challenge detected in the attached Chrome session. Solve it in your browser window and try again."
        );
        assert!(should_demote_auto_selected_cdp_target(&login_err));
        assert!(should_demote_auto_selected_cdp_target(&challenge_err));
    }

    #[test]
    fn auto_selected_cdp_target_is_demoted_for_transport_level_attach_failures() {
        let cdp_err = anyhow!(
            "chrome-devtools-mcp could not reach Chrome's CDP endpoint. request failed: connection refused"
        );
        let dev_browser_err =
            anyhow!("browser.newPage: Timeout 30000ms exceeded while waiting for connectOverCDP");
        let profile_selector_err = anyhow!(
            "profile_email `aviv.s@taboola.com` did not match any live Chrome browser context"
        );
        let page_err = anyhow!(
            "{}",
            r#"ChatGPT send button never became enabled after typing. {"send":"missing"}"#
        );
        assert!(should_demote_auto_selected_cdp_target(&cdp_err));
        assert!(should_demote_auto_selected_cdp_target(&dev_browser_err));
        assert!(should_demote_auto_selected_cdp_target(
            &profile_selector_err
        ));
        assert!(should_demote_auto_selected_cdp_target(&page_err));
    }

    #[test]
    fn browser_sync_cookies_cli_accepts_profile_path() {
        let cli = Cli::try_parse_from([
            "yoetz",
            "browser",
            "sync-cookies",
            "--profile",
            "/tmp/yoetz-browser-profile",
        ])
        .expect("browser sync-cookies args should parse");

        match cli.command {
            Commands::Browser(BrowserArgs {
                command: BrowserCommand::SyncCookies(args),
            }) => {
                assert_eq!(
                    args.profile.as_deref(),
                    Some(Path::new("/tmp/yoetz-browser-profile"))
                );
            }
            _ => panic!("unexpected command parsed"),
        }
    }

    #[test]
    fn config_profile_and_browser_profile_flags_do_not_collide() {
        let cli = Cli::try_parse_from([
            "yoetz",
            "--config-profile",
            "work",
            "browser",
            "sync-cookies",
            "--profile",
            "/tmp/yoetz-browser-profile",
        ])
        .expect("config and browser profile args should parse together");

        assert_eq!(cli.config_profile.as_deref(), Some("work"));
        match cli.command {
            Commands::Browser(BrowserArgs {
                command: BrowserCommand::SyncCookies(args),
            }) => {
                assert_eq!(
                    args.profile.as_deref(),
                    Some(Path::new("/tmp/yoetz-browser-profile"))
                );
            }
            _ => panic!("unexpected command parsed"),
        }
    }

    #[tokio::test]
    async fn run_recipe_via_chrome_devtools_mcp_rejects_non_chatgpt_recipes() {
        // Until the claude/gemini ports land, non-chatgpt recipes through the
        // chrome-devtools-mcp transport surface a clear guidance error rather
        // than silently trying to drive the wrong DOM.
        let recipe_args = BrowserRecipeArgs {
            recipe: PathBuf::from("recipes/claude.yaml"),
            bundle: None,
            profile: None,
            cdp: None,
            browser_id: None,
            vars: vec![],
        };
        let recipe_vars = BTreeMap::new();
        let err = run_recipe_via_chrome_devtools_mcp(
            &test_app_context(),
            &recipe_args,
            &recipe_vars,
            None,
            OutputFormat::Text,
            /* is_chatgpt */ false,
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("chrome-devtools-mcp"));
        assert!(msg.contains("chatgpt"));
    }

    #[tokio::test]
    async fn run_recipe_via_chrome_devtools_mcp_rejects_profile_flag() {
        // --profile is a managed-profile concept from agent-browser; the MCP
        // transport attaches to a running Chrome via --cdp or auto-discovery
        // only. Surface that clearly instead of silently ignoring the flag.
        let recipe_args = BrowserRecipeArgs {
            recipe: PathBuf::from("recipes/chatgpt.yaml"),
            bundle: None,
            profile: Some(PathBuf::from("/tmp/ignored")),
            cdp: None,
            browser_id: None,
            vars: vec![],
        };
        let recipe_vars = BTreeMap::new();
        let err = run_recipe_via_chrome_devtools_mcp(
            &test_app_context(),
            &recipe_args,
            &recipe_vars,
            None,
            OutputFormat::Text,
            /* is_chatgpt */ true,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("--profile"));
        assert!(err.to_string().contains("--cdp"));
    }

    #[tokio::test]
    async fn run_recipe_via_chrome_devtools_mcp_rejects_paste_mode() {
        let recipe_args = BrowserRecipeArgs {
            recipe: PathBuf::from("recipes/chatgpt.yaml"),
            bundle: Some(PathBuf::from("/tmp/bundle.md")),
            profile: None,
            cdp: None,
            browser_id: None,
            vars: vec![],
        };
        let recipe_vars = BTreeMap::from([("paste".to_string(), "true".to_string())]);
        let err = run_recipe_via_chrome_devtools_mcp(
            &test_app_context(),
            &recipe_args,
            &recipe_vars,
            None,
            OutputFormat::Text,
            /* is_chatgpt */ true,
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("paste mode"));
        assert!(msg.contains("file attachment"));
    }

    #[tokio::test]
    async fn run_recipe_via_chrome_devtools_mcp_requires_bundle() {
        let recipe_args = BrowserRecipeArgs {
            recipe: PathBuf::from("recipes/chatgpt.yaml"),
            bundle: None,
            profile: None,
            cdp: None,
            browser_id: None,
            vars: vec![],
        };
        let recipe_vars = BTreeMap::new();
        let err = run_recipe_via_chrome_devtools_mcp(
            &test_app_context(),
            &recipe_args,
            &recipe_vars,
            None,
            OutputFormat::Text,
            /* is_chatgpt */ true,
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--bundle"));
        assert!(msg.contains("paste mode"));
    }

    #[tokio::test]
    async fn run_recipe_via_chrome_devtools_mcp_rejects_invalid_thread_mode() {
        let recipe_args = BrowserRecipeArgs {
            recipe: PathBuf::from("recipes/chatgpt.yaml"),
            bundle: Some(PathBuf::from("/tmp/bundle.md")),
            profile: None,
            cdp: None,
            browser_id: None,
            vars: vec![],
        };
        let recipe_vars = BTreeMap::from([("thread".to_string(), "sideways".to_string())]);
        let err = run_recipe_via_chrome_devtools_mcp(
            &test_app_context(),
            &recipe_args,
            &recipe_vars,
            None,
            OutputFormat::Text,
            /* is_chatgpt */ true,
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("thread"));
        assert!(msg.contains("fresh"));
    }

    #[tokio::test]
    async fn run_recipe_via_chrome_devtools_mcp_rejects_thread_reuse() {
        let recipe_args = BrowserRecipeArgs {
            recipe: PathBuf::from("recipes/chatgpt.yaml"),
            bundle: Some(PathBuf::from("/tmp/bundle.md")),
            profile: None,
            cdp: None,
            browser_id: None,
            vars: vec![],
        };
        let recipe_vars = BTreeMap::from([("thread".to_string(), "reuse".to_string())]);
        let err = run_recipe_via_chrome_devtools_mcp(
            &test_app_context(),
            &recipe_args,
            &recipe_vars,
            None,
            OutputFormat::Text,
            /* is_chatgpt */ true,
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("thread=reuse is no longer supported"));
        assert!(msg.contains("fresh ChatGPT tab"));
    }

    #[test]
    fn resolve_dev_browser_delivery_mode_falls_back_to_paste_when_clipboard_upload_is_unavailable()
    {
        let bundle_path = temp_output_path("yoetz_bundle_text");
        fs::write(&bundle_path, "bundle body").unwrap();
        let recipe_args = BrowserRecipeArgs {
            recipe: PathBuf::from("recipes/chatgpt.yaml"),
            bundle: Some(bundle_path.clone()),
            profile: None,
            cdp: None,
            browser_id: None,
            vars: vec![],
        };
        let recipe_vars = BTreeMap::new();

        let (paste_mode, bundle_text, auto_fallback) =
            resolve_dev_browser_delivery_mode_for_platform(false, &recipe_args, &recipe_vars)
                .unwrap();

        assert!(paste_mode);
        assert!(auto_fallback);
        assert_eq!(bundle_text.as_deref(), Some("bundle body"));
        let _ = fs::remove_file(bundle_path);
    }

    #[test]
    fn build_chatgpt_recipe_spec_uses_shared_contract_fields() {
        let recipe_args = BrowserRecipeArgs {
            recipe: PathBuf::from("recipes/chatgpt.yaml"),
            bundle: Some(PathBuf::from("/tmp/bundle.md")),
            profile: None,
            cdp: None,
            browser_id: None,
            vars: vec![],
        };
        let recipe_vars = BTreeMap::from([
            ("model".to_string(), "pro".to_string()),
            ("prompt".to_string(), "Review this repo".to_string()),
            ("browser_context_id".to_string(), "ctx-123".to_string()),
            ("profile_email".to_string(), "user@example.com".to_string()),
            ("run_id".to_string(), "run-123".to_string()),
            ("wait_timeout_ms".to_string(), "2400000".to_string()),
            ("wait_interval_ms".to_string(), "45000".to_string()),
            ("extended".to_string(), "false".to_string()),
        ]);

        let spec = build_chatgpt_recipe_spec(&recipe_args, &recipe_vars).unwrap();
        assert_eq!(spec.bundle_path, Some(PathBuf::from("/tmp/bundle.md")));
        assert_eq!(spec.model, "pro");
        assert_eq!(spec.prompt, "Review this repo");
        assert_eq!(spec.browser_context_id.as_deref(), Some("ctx-123"));
        assert_eq!(spec.profile_email.as_deref(), Some("user@example.com"));
        assert_eq!(spec.run_id, "run-123");
        assert_eq!(spec.wait_timeout_ms, 2_400_000);
        assert_eq!(spec.wait_interval_ms, 45_000);
        assert!(spec.disable_extended);
    }

    #[test]
    fn chatgpt_recipe_output_contract_event_includes_divergence_metadata() {
        let output = crate::chatgpt_recipe::ChatgptRecipeOutput {
            transport: "dev-browser".to_string(),
            backend: "dev-browser".to_string(),
            response: "ok".to_string(),
            model_used: Some("gpt-5-4-pro".to_string()),
            warnings: vec!["clipboard fallback".to_string()],
            fallback_used: true,
            delivery_mode: crate::chatgpt_recipe::ChatgptDeliveryMode::Paste,
            auto_paste_fallback: true,
        };

        let event = output.to_recipe_complete_event();
        assert_eq!(event["type"], "recipe_complete");
        assert_eq!(event["transport"], "dev-browser");
        assert_eq!(event["delivery_mode"], "paste");
        assert_eq!(event["auto_paste_fallback"], true);
        assert_eq!(event["warnings"], json!(["clipboard fallback"]));
    }

    #[test]
    fn run_recipe_via_dev_browser_rejects_invalid_thread_mode_before_install_check() {
        let recipe_args = BrowserRecipeArgs {
            recipe: PathBuf::from("recipes/chatgpt.yaml"),
            bundle: Some(PathBuf::from("/tmp/bundle.md")),
            profile: None,
            cdp: None,
            browser_id: None,
            vars: vec![],
        };
        let recipe_vars = BTreeMap::from([("thread".to_string(), "sideways".to_string())]);
        let err = run_recipe_via_dev_browser(
            &test_app_context(),
            &recipe_args,
            &recipe_vars,
            None,
            OutputFormat::Text,
            /* is_chatgpt */ true,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("thread"));
        assert!(msg.contains("fresh"));
    }

    #[test]
    fn run_recipe_via_dev_browser_rejects_thread_reuse_before_install_check() {
        let recipe_args = BrowserRecipeArgs {
            recipe: PathBuf::from("recipes/chatgpt.yaml"),
            bundle: Some(PathBuf::from("/tmp/bundle.md")),
            profile: None,
            cdp: None,
            browser_id: None,
            vars: vec![],
        };
        let recipe_vars = BTreeMap::from([("thread".to_string(), "reuse".to_string())]);
        let err = run_recipe_via_dev_browser(
            &test_app_context(),
            &recipe_args,
            &recipe_vars,
            None,
            OutputFormat::Text,
            /* is_chatgpt */ true,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("thread=reuse is no longer supported"));
        assert!(msg.contains("fresh ChatGPT tab"));
    }

    #[test]
    fn recipe_transport_error_detail_preserves_error_chain() {
        let err = anyhow::anyhow!("browserType.connectOverCDP: Timeout 30000ms exceeded")
            .context("dev-browser could not connect to Chrome");
        let detail = recipe_transport_error_detail(&err);
        assert!(detail.contains("dev-browser could not connect to Chrome"));
        assert!(detail.contains("browserType.connectOverCDP: Timeout 30000ms exceeded"));
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
