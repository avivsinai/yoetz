use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose, Engine as _};
use clap::{Args, Parser, Subcommand};
use jsonschema::JSONSchema;
use litellm_rs::{
    ChatContentPart, ChatContentPartFile, ChatContentPartImageUrl, ChatContentPartText, ChatFile,
    ChatImageUrl, ChatMessageContent, ChatRequest, ImageData, LiteLLM,
    ProviderConfig as LiteProviderConfig, ProviderKind as LiteProviderKind,
};
use serde::Serialize;
use serde_json::Value;
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

mod browser;
mod budget;
mod commands;
mod http;
mod providers;
mod registry;

use yoetz_core::config::Config;
use yoetz_core::media::MediaInput;
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};
use yoetz_core::registry::ModelRegistry;
use yoetz_core::session::{list_sessions, write_json as write_json_file};
use yoetz_core::types::{ArtifactPaths, PricingEstimate, Usage};

use http::send_json;

const DEFAULT_MAX_OUTPUT_TOKENS: usize = 1024;

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
    output_final: Option<PathBuf>,

    #[arg(long, global = true)]
    output_schema: Option<PathBuf>,

    #[arg(long, global = true)]
    profile: Option<String>,

    #[arg(long, global = true, default_value = "60")]
    timeout_secs: u64,

    #[command(subcommand)]
    command: Commands,
}

struct AppContext {
    config: Config,
    client: reqwest::Client,
    litellm: LiteLLM,
    output_final: Option<PathBuf>,
    output_schema: Option<PathBuf>,
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
    #[arg(short, long)]
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

    #[arg(long, value_name = "PATH_OR_URL")]
    video: Option<String>,

    #[arg(long, value_name = "json|text")]
    response_format: Option<String>,

    #[arg(long)]
    response_schema: Option<PathBuf>,

    #[arg(long)]
    response_schema_name: Option<String>,
}

#[derive(Args)]
struct BundleArgs {
    #[arg(short, long)]
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
}

#[derive(Args)]
struct CouncilArgs {
    #[arg(short, long)]
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
    #[arg(short, long)]
    prompt: Option<String>,

    #[arg(long)]
    prompt_file: Option<PathBuf>,

    #[arg(long)]
    provider: Option<String>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long, value_name = "PATH_OR_URL")]
    image: Vec<String>,

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
    #[arg(short, long)]
    prompt: Option<String>,

    #[arg(long)]
    prompt_file: Option<PathBuf>,

    #[arg(long)]
    provider: Option<String>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long, value_name = "PATH_OR_URL")]
    image: Vec<String>,

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
    #[arg(long)]
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
}

#[derive(Args)]
struct ReviewFileArgs {
    #[arg(long)]
    path: PathBuf,

    #[arg(long)]
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
    List,
    Sync,
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
    let cli = Cli::parse();
    let format = resolve_format(cli.format.as_deref())?;
    let config = Config::load_with_profile(cli.profile.as_deref())?;
    let client = build_client(cli.timeout_secs)?;
    let litellm = build_litellm(&config, client.clone())?;
    let ctx = AppContext {
        config,
        client,
        litellm,
        output_final: cli.output_final,
        output_schema: cli.output_schema,
    };

    match cli.command {
        Commands::Ask(args) => commands::ask::handle_ask(&ctx, args, format).await,
        Commands::Bundle(args) => commands::bundle::handle_bundle(&ctx, args, format),
        Commands::Status => handle_status(&ctx, format),
        Commands::Session(args) => handle_session(&ctx, args, format),
        Commands::Models(args) => commands::models::handle_models(&ctx, args, format).await,
        Commands::Pricing(args) => commands::pricing::handle_pricing(&ctx, args, format).await,
        Commands::Browser(args) => handle_browser(args, format),
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

fn handle_browser(args: BrowserArgs, format: OutputFormat) -> Result<()> {
    match args.command {
        BrowserCommand::Exec(exec) => {
            let stdout = browser::run_agent_browser(exec.args, format)?;
            print!("{stdout}");
            Ok(())
        }
        BrowserCommand::Recipe(recipe_args) => {
            let content = fs::read_to_string(&recipe_args.recipe)
                .with_context(|| format!("read recipe {}", recipe_args.recipe.display()))?;
            let recipe: browser::Recipe = serde_yaml::from_str(&content)?;

            let bundle_text = if let Some(path) = recipe_args.bundle.as_ref() {
                Some(fs::read_to_string(path)?)
            } else {
                None
            };

            let ctx = browser::RecipeContext {
                bundle_path: recipe_args.bundle.map(|p| p.to_string_lossy().to_string()),
                bundle_text,
            };

            browser::run_recipe(recipe, ctx, format)
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

fn add_usage(mut total: Usage, usage: &Usage) -> Usage {
    if let Some(input) = usage.input_tokens {
        total.input_tokens = Some(total.input_tokens.unwrap_or(0) + input);
    }
    if let Some(output) = usage.output_tokens {
        total.output_tokens = Some(total.output_tokens.unwrap_or(0) + output);
    }
    if let Some(total_tokens) = usage.total_tokens {
        total.total_tokens = Some(total.total_tokens.unwrap_or(0) + total_tokens);
    }
    if let Some(cost) = usage.cost_usd {
        total.cost_usd = Some(total.cost_usd.unwrap_or(0.0) + cost);
    }
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
    let compiled = JSONSchema::compile(&schema_json)
        .map_err(|e| anyhow!("invalid schema {}: {e}", path.display()))?;
    if let Err(errors) = compiled.validate(value) {
        let messages = errors.map(|e| e.to_string()).collect::<Vec<_>>().join("; ");
        return Err(anyhow!(
            "output does not match schema {}: {}",
            path.display(),
            messages
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

fn parse_media_inputs(values: &[String]) -> Result<Vec<MediaInput>> {
    let mut out = Vec::new();
    for value in values {
        out.push(parse_media_input(value)?);
    }
    Ok(out)
}

fn parse_media_input(value: &str) -> Result<MediaInput> {
    if value.starts_with("http://") || value.starts_with("https://") {
        return MediaInput::from_url(value, None);
    }
    MediaInput::from_path(PathBuf::from(value).as_path())
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

async fn call_litellm(
    litellm: &LiteLLM,
    provider: Option<&str>,
    model: &str,
    prompt: &str,
    temperature: f32,
    max_output_tokens: usize,
    response_format: Option<Value>,
    images: &[MediaInput],
    video: Option<&MediaInput>,
) -> Result<CallResult> {
    let model_spec = build_model_spec(provider, model)?;
    let mut req = ChatRequest::new(model_spec)
        .temperature(temperature)
        .max_tokens(max_output_tokens as u32);
    req.response_format = response_format;

    if images.is_empty() && video.is_none() {
        req = req.message("user", prompt);
    } else {
        let mut parts = Vec::new();
        parts.push(ChatContentPart::Text(ChatContentPartText {
            kind: "text".to_string(),
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

fn build_model_spec(provider: Option<&str>, model: &str) -> Result<String> {
    let Some(provider) = provider else {
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
                        "openrouter models must be namespaced (e.g. openai/gpt-4o, anthropic/claude-3-5-sonnet)"
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
            "openrouter models must be namespaced (e.g. openai/gpt-4o, anthropic/claude-3-5-sonnet)"
        ));
    }
    Ok(format!("{provider}/{model}"))
}

fn resolve_max_output_tokens(requested: Option<usize>, config: &Config) -> usize {
    requested
        .or(config.defaults.max_output_tokens)
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS)
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

fn usage_from_litellm(usage: litellm_rs::Usage) -> Usage {
    Usage {
        input_tokens: usage.prompt_tokens.map(|v| v as usize),
        output_tokens: usage.completion_tokens.map(|v| v as usize),
        total_tokens: usage.total_tokens.map(|v| v as usize),
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
        kind: "image_url".to_string(),
        image_url: ChatImageUrl::Url(url),
    }))
}

fn media_to_file_part(media: &MediaInput) -> Result<ChatContentPart> {
    let url = media.as_data_url()?;
    Ok(ChatContentPart::File(ChatContentPartFile {
        kind: "file".to_string(),
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
        let filename = format!("image_{idx}.png");
        let path = output_dir.join(filename);
        if let Some(b64) = image.b64_json.as_ref() {
            let bytes = general_purpose::STANDARD
                .decode(b64.as_bytes())
                .context("decode image base64")?;
            std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
        } else if let Some(url) = image.url.as_ref() {
            let bytes = client.get(url).send().await?.bytes().await?;
            std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
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
