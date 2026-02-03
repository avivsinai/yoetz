use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::fs;
use std::io::{self, Read, IsTerminal};
use std::path::PathBuf;
use std::process::Command;

mod browser;
mod budget;
mod registry;

use yoetz_core::bundle::{build_bundle, estimate_tokens, BundleOptions};
use yoetz_core::config::Config;
use yoetz_core::output::{write_json, write_jsonl_event, OutputFormat};
use yoetz_core::session::{create_session_dir, list_sessions, write_json as write_json_file, write_text};
use yoetz_core::types::{ArtifactPaths, BundleResult, PricingEstimate, RunResult, Usage};

#[derive(Parser)]
#[command(name = "yoetz", version, about = "Fast, agent-friendly LLM council tool")]
struct Cli {
    #[arg(long, global = true)]
    format: Option<String>,

    #[arg(long, global = true)]
    output_final: Option<PathBuf>,

    #[arg(long, global = true)]
    output_schema: Option<PathBuf>,

    #[arg(long, global = true)]
    profile: Option<String>,

    #[command(subcommand)]
    command: Commands,
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

    #[arg(long, default_value = "1024")]
    max_output_tokens: usize,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    max_cost_usd: Option<f64>,

    #[arg(long)]
    daily_budget_usd: Option<f64>,
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

    #[arg(long, default_value = "1024")]
    max_output_tokens: usize,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    max_cost_usd: Option<f64>,

    #[arg(long)]
    daily_budget_usd: Option<f64>,
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

    #[arg(long, default_value = "1024")]
    max_output_tokens: usize,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    max_cost_usd: Option<f64>,

    #[arg(long)]
    daily_budget_usd: Option<f64>,
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

    #[arg(long, default_value = "1024")]
    max_output_tokens: usize,

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

#[derive(Debug, Deserialize)]
struct OpenAIChatResponse {
    id: Option<String>,
    choices: Vec<OpenAIChoice>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    message: OpenAIMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAIMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: Option<usize>,
    completion_tokens: Option<usize>,
    total_tokens: Option<usize>,
}

#[derive(Debug, Serialize)]
struct JsonlFinal<'a> {
    r#type: &'a str,
    data: &'a RunResult,
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

    match cli.command {
        Commands::Ask(args) => handle_ask(args, format, cli.output_final, cli.output_schema).await,
        Commands::Bundle(args) => handle_bundle(args, format),
        Commands::Status => handle_status(format),
        Commands::Session(args) => handle_session(args, format),
        Commands::Models(args) => handle_models(args, format).await,
        Commands::Pricing(args) => handle_pricing(args, format).await,
        Commands::Browser(args) => handle_browser(args, format),
        Commands::Council(args) => handle_council(args, format).await,
        Commands::Apply(args) => handle_apply(args),
        Commands::Review(args) => handle_review(args, format).await,
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

async fn handle_ask(
    args: AskArgs,
    format: OutputFormat,
    output_final: Option<PathBuf>,
    _output_schema: Option<PathBuf>,
) -> Result<()> {
    let prompt = resolve_prompt(args.prompt.clone(), args.prompt_file.clone())?;
    let config = Config::load().unwrap_or_default();

    let include_files = args.files.clone();
    let exclude_files = args.exclude.clone();

    let bundle = if include_files.is_empty() {
        None
    } else {
        let options = BundleOptions {
            include: include_files,
            exclude: exclude_files,
            max_file_bytes: args.max_file_bytes,
            max_total_bytes: args.max_total_bytes,
            ..Default::default()
        };
        Some(build_bundle(&prompt, options)?)
    };

    let session = create_session_dir()?;
    let mut artifacts = ArtifactPaths {
        session_dir: session.path.to_string_lossy().to_string(),
        ..Default::default()
    };

    if let Some(bundle_ref) = &bundle {
        let bundle_json = session.path.join("bundle.json");
        let bundle_md = session.path.join("bundle.md");
        write_json_file(&bundle_json, bundle_ref)?;
        write_text(&bundle_md, &render_bundle_md(bundle_ref))?;
        artifacts.bundle_json = Some(bundle_json.to_string_lossy().to_string());
        artifacts.bundle_md = Some(bundle_md.to_string_lossy().to_string());
    }

    let model_id = args.model.clone().or(config.defaults.model.clone());
    let provider_id = args.provider.clone().or(config.defaults.provider.clone());
    let registry_cache = registry::load_registry_cache().ok().flatten();
    let input_tokens = bundle
        .as_ref()
        .map(|b| b.stats.estimated_tokens)
        .unwrap_or_else(|| estimate_tokens(prompt.len()));
    let output_tokens = args.max_output_tokens;
    let pricing = if let Some(model_id) = model_id.as_deref() {
        registry::estimate_pricing(registry_cache.as_ref(), model_id, input_tokens, output_tokens)?
    } else {
        PricingEstimate::default()
    };

    let mut ledger = None;
    if args.max_cost_usd.is_some() || args.daily_budget_usd.is_some() {
        ledger = Some(budget::ensure_budget(
            pricing.estimate_usd,
            args.max_cost_usd,
            args.daily_budget_usd,
        )?);
    }

    let model_prompt = if let Some(bundle_ref) = &bundle {
        render_bundle_md(bundle_ref)
    } else {
        prompt.clone()
    };

    let (content, mut usage, response_id, header_cost) = if args.dry_run {
        (
            "(dry-run) no provider call executed".to_string(),
            Usage::default(),
            None,
            None,
        )
    } else {
        let provider = provider_id
            .as_deref()
            .ok_or_else(|| anyhow!("provider is required"))?;
        let model = model_id
            .as_deref()
            .ok_or_else(|| anyhow!("model is required"))?;
        let result = call_openai_compatible(
            &model_prompt,
            &config,
            provider,
            model,
            args.temperature,
            args.max_output_tokens,
        )
        .await?;
        (result.content, result.usage, result.response_id, result.header_cost)
    };

    if usage.cost_usd.is_none() {
        usage.cost_usd = header_cost;
    }

    if usage.cost_usd.is_none() {
        if let Some(provider) = provider_id.as_deref() {
            if provider == "openrouter" {
                if let Some(id) = response_id.as_deref() {
                    if let Ok(cost) = fetch_openrouter_cost(&config, id).await {
                        usage.cost_usd = cost;
                    }
                }
            }
        }
    }

    if let Some(ledger) = ledger {
        if let Some(spend) = usage.cost_usd.or(pricing.estimate_usd) {
            let _ = budget::record_spend(ledger, spend);
        }
    }

    let result = RunResult {
        id: session.id,
        model: model_id,
        provider: provider_id,
        bundle,
        pricing,
        usage,
        content,
        artifacts,
    };

    let response_json = PathBuf::from(&result.artifacts.session_dir).join("response.json");
    write_json_file(&response_json, &result)?;

    if let Some(path) = output_final {
        write_json_file(&path, &result)?;
    }

    match format {
        OutputFormat::Json => write_json(&result),
        OutputFormat::Jsonl => {
            let event = JsonlFinal { r#type: "final", data: &result };
            write_jsonl_event(&event)
        }
        OutputFormat::Text => {
            println!("{}", result.content);
            Ok(())
        }
        OutputFormat::Markdown => {
            println!("{}", result.content);
            Ok(())
        }
    }
}

fn handle_bundle(args: BundleArgs, format: OutputFormat) -> Result<()> {
    let prompt = resolve_prompt(args.prompt, args.prompt_file)?;
    let options = BundleOptions {
        include: args.files,
        exclude: args.exclude,
        max_file_bytes: args.max_file_bytes,
        max_total_bytes: args.max_total_bytes,
        ..Default::default()
    };

    let bundle = build_bundle(&prompt, options)?;
    let session = create_session_dir()?;

    let bundle_json = session.path.join("bundle.json");
    let bundle_md = session.path.join("bundle.md");

    write_json_file(&bundle_json, &bundle)?;
    write_text(&bundle_md, &render_bundle_md(&bundle))?;

    let result = BundleResult {
        id: session.id,
        bundle,
        artifacts: ArtifactPaths {
            session_dir: session.path.to_string_lossy().to_string(),
            bundle_json: Some(bundle_json.to_string_lossy().to_string()),
            bundle_md: Some(bundle_md.to_string_lossy().to_string()),
            response_json: None,
        },
    };

    match format {
        OutputFormat::Json => write_json(&result),
        OutputFormat::Jsonl => write_jsonl_event(&result),
        OutputFormat::Text => {
            println!("Bundle created at {}", result.artifacts.session_dir);
            Ok(())
        }
        OutputFormat::Markdown => {
            println!("Bundle created at `{}`", result.artifacts.session_dir);
            Ok(())
        }
    }
}

fn handle_status(format: OutputFormat) -> Result<()> {
    let sessions = list_sessions()?;
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => write_json(&sessions),
        OutputFormat::Text | OutputFormat::Markdown => {
            for s in sessions {
                println!("{}\t{}", s.id, s.path.display());
            }
            Ok(())
        }
    }
}

fn handle_session(args: SessionArgs, format: OutputFormat) -> Result<()> {
    let base = yoetz_core::session::session_base_dir();
    let path = base.join(&args.id);
    if !path.exists() {
        return Err(anyhow!("session not found: {}", args.id));
    }
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => write_json(&path),
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
                bundle_path: recipe_args
                    .bundle
                    .map(|p| p.to_string_lossy().to_string()),
                bundle_text,
            };

            browser::run_recipe(recipe, ctx, format)
        }
    }
}

async fn handle_council(args: CouncilArgs, format: OutputFormat) -> Result<()> {
    let prompt = resolve_prompt(args.prompt.clone(), args.prompt_file.clone())?;
    let config = Config::load().unwrap_or_default();

    if args.models.is_empty() {
        return Err(anyhow!("at least one model is required"));
    }

    let provider = args
        .provider
        .clone()
        .or(config.defaults.provider.clone())
        .ok_or_else(|| anyhow!("provider is required"))?;

    let include_files = args.files.clone();
    let exclude_files = args.exclude.clone();

    let bundle = if include_files.is_empty() {
        None
    } else {
        let options = BundleOptions {
            include: include_files,
            exclude: exclude_files,
            max_file_bytes: args.max_file_bytes,
            max_total_bytes: args.max_total_bytes,
            ..Default::default()
        };
        Some(build_bundle(&prompt, options)?)
    };

    let registry_cache = registry::load_registry_cache().ok().flatten();
    let input_tokens = bundle
        .as_ref()
        .map(|b| b.stats.estimated_tokens)
        .unwrap_or_else(|| estimate_tokens(prompt.len()));
    let output_tokens = args.max_output_tokens;

    let mut per_model = Vec::new();
    let mut estimate_sum = 0.0;
    let mut estimate_complete = true;
    for model in &args.models {
        let estimate = registry::estimate_pricing(
            registry_cache.as_ref(),
            model,
            input_tokens,
            output_tokens,
        )?;
        if let Some(cost) = estimate.estimate_usd {
            estimate_sum += cost;
        } else {
            estimate_complete = false;
        }
        per_model.push(ModelEstimate {
            model: model.clone(),
            estimate_usd: estimate.estimate_usd,
        });
    }
    let total_estimate = if estimate_complete {
        Some(estimate_sum)
    } else {
        None
    };

    let mut ledger = None;
    if args.max_cost_usd.is_some() || args.daily_budget_usd.is_some() {
        ledger = Some(budget::ensure_budget(
            total_estimate,
            args.max_cost_usd,
            args.daily_budget_usd,
        )?);
    }

    let session = create_session_dir()?;
    let mut artifacts = ArtifactPaths {
        session_dir: session.path.to_string_lossy().to_string(),
        ..Default::default()
    };

    if let Some(bundle_ref) = &bundle {
        let bundle_json = session.path.join("bundle.json");
        let bundle_md = session.path.join("bundle.md");
        write_json_file(&bundle_json, bundle_ref)?;
        write_text(&bundle_md, &render_bundle_md(bundle_ref))?;
        artifacts.bundle_json = Some(bundle_json.to_string_lossy().to_string());
        artifacts.bundle_md = Some(bundle_md.to_string_lossy().to_string());
    }

    let mut results = Vec::new();
    let mut total_usage = Usage::default();
    let model_prompt = if let Some(bundle_ref) = &bundle {
        render_bundle_md(bundle_ref)
    } else {
        prompt.clone()
    };

    if args.dry_run {
        for model in &args.models {
            results.push(CouncilModelResult {
                model: model.clone(),
                content: "(dry-run) no provider call executed".to_string(),
                usage: Usage::default(),
                pricing: registry::estimate_pricing(
                    registry_cache.as_ref(),
                    model,
                    input_tokens,
                    output_tokens,
                )?,
                response_id: None,
            });
        }
    } else {
        let mut join_set = tokio::task::JoinSet::new();
        for model in args.models.clone() {
            let prompt = model_prompt.clone();
            let config = config.clone();
            let provider = provider.clone();
            let temperature = args.temperature;
            let max_output_tokens = args.max_output_tokens;
            join_set.spawn(async move {
                let call = call_openai_compatible(
                    &prompt,
                    &config,
                    &provider,
                    &model,
                    temperature,
                    max_output_tokens,
                )
                .await?;
                Ok::<_, anyhow::Error>((model, call))
            });
        }

        while let Some(res) = join_set.join_next().await {
            let (model, call) = res??;
            let mut usage = call.usage;
            if usage.cost_usd.is_none() {
                usage.cost_usd = call.header_cost;
            }
            if usage.cost_usd.is_none() && provider == "openrouter" {
                if let Some(id) = call.response_id.as_deref() {
                    if let Ok(cost) = fetch_openrouter_cost(&config, id).await {
                        usage.cost_usd = cost;
                    }
                }
            }

            total_usage = add_usage(total_usage, &usage);

            let pricing = registry::estimate_pricing(
                registry_cache.as_ref(),
                &model,
                input_tokens,
                output_tokens,
            )?;

            results.push(CouncilModelResult {
                model,
                content: call.content,
                usage,
                pricing,
                response_id: call.response_id,
            });
        }
    }

    if let Some(ledger) = ledger {
        let mut spend = 0.0;
        let mut has_spend = false;
        for r in &results {
            if let Some(cost) = r.usage.cost_usd.or(r.pricing.estimate_usd) {
                spend += cost;
                has_spend = true;
            }
        }
        if has_spend {
            let _ = budget::record_spend(ledger, spend);
        }
    }

    let council = CouncilResult {
        id: session.id,
        provider,
        bundle,
        results,
        pricing: CouncilPricing {
            estimate_usd_total: total_estimate,
            per_model,
        },
        usage: total_usage,
        artifacts,
    };

    let response_json = PathBuf::from(&council.artifacts.session_dir).join("council.json");
    write_json_file(&response_json, &council)?;

    match format {
        OutputFormat::Json => write_json(&council),
        OutputFormat::Jsonl => write_jsonl_event(&council),
        OutputFormat::Text => {
            for r in &council.results {
                println!("## {}\n{}\n", r.model, r.content);
            }
            Ok(())
        }
        OutputFormat::Markdown => {
            for r in &council.results {
                println!("## {}\n{}\n", r.model, r.content);
            }
            Ok(())
        }
    }
}

fn handle_apply(args: ApplyArgs) -> Result<()> {
    let patch = if let Some(path) = args.patch_file {
        fs::read_to_string(path)?
    } else {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        buf
    };

    if patch.trim().is_empty() {
        return Err(anyhow!("patch is empty"));
    }

    let mut tmp = tempfile::NamedTempFile::new()?;
    use std::io::Write;
    tmp.write_all(patch.as_bytes())?;

    let mut cmd = Command::new("git");
    cmd.arg("apply");
    if args.check {
        cmd.arg("--check");
    }
    if args.reverse {
        cmd.arg("--reverse");
    }
    cmd.arg(tmp.path());

    let status = cmd.status()?;
    if !status.success() {
        return Err(anyhow!("git apply failed"));
    }

    if args.check {
        println!("Patch OK");
    } else {
        println!("Patch applied");
    }
    Ok(())
}

async fn handle_review(args: ReviewArgs, format: OutputFormat) -> Result<()> {
    match args.command {
        ReviewCommand::Diff(diff_args) => handle_review_diff(diff_args, format).await,
        ReviewCommand::File(file_args) => handle_review_file(file_args, format).await,
    }
}

async fn handle_review_diff(args: ReviewDiffArgs, format: OutputFormat) -> Result<()> {
    let config = Config::load().unwrap_or_default();
    let provider = args
        .provider
        .clone()
        .or(config.defaults.provider.clone())
        .ok_or_else(|| anyhow!("provider is required"))?;
    let model = args
        .model
        .clone()
        .or(config.defaults.model.clone())
        .ok_or_else(|| anyhow!("model is required"))?;

    let diff = git_diff(args.staged, &args.paths)?;
    if diff.trim().is_empty() {
        return Err(anyhow!("diff is empty"));
    }

    let review_prompt = build_review_diff_prompt(&diff, args.prompt.as_deref());
    let input_tokens = estimate_tokens(review_prompt.len());
    let pricing = registry::estimate_pricing(
        registry::load_registry_cache().ok().flatten().as_ref(),
        &model,
        input_tokens,
        args.max_output_tokens,
    )?;

    let mut ledger = None;
    if args.max_cost_usd.is_some() || args.daily_budget_usd.is_some() {
        ledger = Some(budget::ensure_budget(
            pricing.estimate_usd,
            args.max_cost_usd,
            args.daily_budget_usd,
        )?);
    }

    let session = create_session_dir()?;
    let artifacts = ArtifactPaths {
        session_dir: session.path.to_string_lossy().to_string(),
        ..Default::default()
    };
    let review_input_path = session.path.join("review_input.txt");
    write_text(&review_input_path, &review_prompt)?;

    let (content, mut usage, response_id, header_cost) = if args.dry_run {
        (
            "(dry-run) no provider call executed".to_string(),
            Usage::default(),
            None,
            None,
        )
    } else {
        let result = call_openai_compatible(
            &review_prompt,
            &config,
            &provider,
            &model,
            args.temperature,
            args.max_output_tokens,
        )
        .await?;
        (result.content, result.usage, result.response_id, result.header_cost)
    };

    if usage.cost_usd.is_none() {
        usage.cost_usd = header_cost;
    }
    if usage.cost_usd.is_none() && provider == "openrouter" {
        if let Some(id) = response_id.as_deref() {
            if let Ok(cost) = fetch_openrouter_cost(&config, id).await {
                usage.cost_usd = cost;
            }
        }
    }

    if let Some(ledger) = ledger {
        if let Some(spend) = usage.cost_usd.or(pricing.estimate_usd) {
            let _ = budget::record_spend(ledger, spend);
        }
    }

    let mut result = ReviewResult {
        id: session.id,
        provider,
        model,
        pricing,
        usage,
        content,
        artifacts,
    };

    let response_json = PathBuf::from(&result.artifacts.session_dir).join("review.json");
    write_json_file(&response_json, &result)?;
    result.artifacts.response_json = Some(response_json.to_string_lossy().to_string());

    match format {
        OutputFormat::Json => write_json(&result),
        OutputFormat::Jsonl => write_jsonl_event(&result),
        OutputFormat::Text => {
            println!("{}", result.content);
            Ok(())
        }
        OutputFormat::Markdown => {
            println!("{}", result.content);
            Ok(())
        }
    }
}

async fn handle_review_file(args: ReviewFileArgs, format: OutputFormat) -> Result<()> {
    let config = Config::load().unwrap_or_default();
    let provider = args
        .provider
        .clone()
        .or(config.defaults.provider.clone())
        .ok_or_else(|| anyhow!("provider is required"))?;
    let model = args
        .model
        .clone()
        .or(config.defaults.model.clone())
        .ok_or_else(|| anyhow!("model is required"))?;

    let max_file_bytes = args.max_file_bytes.unwrap_or(200_000);
    let (content, truncated) = read_text_file(args.path.as_path(), max_file_bytes)?;
    let review_prompt = build_review_file_prompt(args.path.as_path(), &content, truncated, args.prompt.as_deref());
    let input_tokens = estimate_tokens(review_prompt.len());
    let pricing = registry::estimate_pricing(
        registry::load_registry_cache().ok().flatten().as_ref(),
        &model,
        input_tokens,
        args.max_output_tokens,
    )?;

    let mut ledger = None;
    if args.max_cost_usd.is_some() || args.daily_budget_usd.is_some() {
        ledger = Some(budget::ensure_budget(
            pricing.estimate_usd,
            args.max_cost_usd,
            args.daily_budget_usd,
        )?);
    }

    let session = create_session_dir()?;
    let artifacts = ArtifactPaths {
        session_dir: session.path.to_string_lossy().to_string(),
        ..Default::default()
    };
    let review_input_path = session.path.join("review_input.txt");
    write_text(&review_input_path, &review_prompt)?;

    let (output, mut usage, response_id, header_cost) = if args.dry_run {
        (
            "(dry-run) no provider call executed".to_string(),
            Usage::default(),
            None,
            None,
        )
    } else {
        let result = call_openai_compatible(
            &review_prompt,
            &config,
            &provider,
            &model,
            args.temperature,
            args.max_output_tokens,
        )
        .await?;
        (result.content, result.usage, result.response_id, result.header_cost)
    };

    if usage.cost_usd.is_none() {
        usage.cost_usd = header_cost;
    }
    if usage.cost_usd.is_none() && provider == "openrouter" {
        if let Some(id) = response_id.as_deref() {
            if let Ok(cost) = fetch_openrouter_cost(&config, id).await {
                usage.cost_usd = cost;
            }
        }
    }

    if let Some(ledger) = ledger {
        if let Some(spend) = usage.cost_usd.or(pricing.estimate_usd) {
            let _ = budget::record_spend(ledger, spend);
        }
    }

    let mut result = ReviewResult {
        id: session.id,
        provider,
        model,
        pricing,
        usage,
        content: output,
        artifacts,
    };

    let response_json = PathBuf::from(&result.artifacts.session_dir).join("review.json");
    write_json_file(&response_json, &result)?;
    result.artifacts.response_json = Some(response_json.to_string_lossy().to_string());

    match format {
        OutputFormat::Json => write_json(&result),
        OutputFormat::Jsonl => write_jsonl_event(&result),
        OutputFormat::Text => {
            println!("{}", result.content);
            Ok(())
        }
        OutputFormat::Markdown => {
            println!("{}", result.content);
            Ok(())
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

fn build_review_file_prompt(path: &std::path::Path, content: &str, truncated: bool, extra_prompt: Option<&str>) -> String {
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
    let data = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let truncated = data.len() > max_bytes;
    let boundary = if truncated {
        floor_char_boundary(&data, max_bytes)
    } else {
        data.len()
    };
    let slice = &data[..boundary];
    if slice.contains(&0) {
        return Err(anyhow!("file appears to be binary"));
    }
    let text = std::str::from_utf8(slice)
        .map_err(|_| anyhow!("file is not valid UTF-8"))?;
    Ok((text.to_string(), truncated))
}

fn floor_char_boundary(data: &[u8], index: usize) -> usize {
    if index >= data.len() {
        return data.len();
    }
    let mut i = index;
    while i > 0 && (data[i] & 0xC0) == 0x80 {
        i -= 1;
    }
    i
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

async fn handle_models(args: ModelsArgs, format: OutputFormat) -> Result<()> {
    match args.command {
        ModelsCommand::List => {
            let registry = registry::load_registry_cache()?.unwrap_or_default();
            match format {
                OutputFormat::Json | OutputFormat::Jsonl => write_json(&registry),
                OutputFormat::Text | OutputFormat::Markdown => {
                    for model in registry.models {
                        println!("{}", model.id);
                    }
                    Ok(())
                }
            }
        }
        ModelsCommand::Sync => {
            let config = Config::load().unwrap_or_default();
            let registry = registry::fetch_registry(&config).await?;
            let path = registry::save_registry_cache(&registry)?;
            let payload = serde_json::json!({
                "saved_to": path,
                "model_count": registry.models.len(),
            });
            match format {
                OutputFormat::Json | OutputFormat::Jsonl => write_json(&payload),
                OutputFormat::Text | OutputFormat::Markdown => {
                    println!(
                        "Saved {} models to {}",
                        registry.models.len(),
                        path.display()
                    );
                    Ok(())
                }
            }
        }
    }
}

async fn handle_pricing(args: PricingArgs, format: OutputFormat) -> Result<()> {
    match args.command {
        PricingCommand::Estimate(e) => {
            let registry = registry::load_registry_cache()?.unwrap_or_default();
            let estimate = registry::estimate_pricing(
                Some(&registry),
                &e.model,
                e.input_tokens,
                e.output_tokens,
            )?;
            match format {
                OutputFormat::Json | OutputFormat::Jsonl => write_json(&estimate),
                OutputFormat::Text | OutputFormat::Markdown => {
                    if let Some(cost) = estimate.estimate_usd {
                        println!("Estimated cost: ${:.6}", cost);
                    } else {
                        println!("Estimate unavailable");
                    }
                    Ok(())
                }
            }
        }
    }
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
    Err(anyhow!("prompt is required (--prompt, --prompt-file, or stdin)"))
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
            out.push_str("```\n");
            out.push_str(content);
            if file.truncated {
                out.push_str("\n... [truncated]\n");
            }
            out.push_str("```\n\n");
        } else if file.is_binary {
            out.push_str("(binary file omitted)\n\n");
        }
    }
    out
}

async fn call_openai_compatible(
    prompt: &str,
    config: &Config,
    provider: &str,
    model: &str,
    temperature: f32,
    max_output_tokens: usize,
) -> Result<CallResult> {
    let provider_cfg = config.providers.get(provider);

    let base_url = provider_cfg
        .and_then(|p| p.base_url.clone())
        .or_else(|| default_base_url(provider))
        .ok_or_else(|| anyhow!("base_url not found for provider {provider}"))?;

    let api_key_env = provider_cfg
        .and_then(|p| p.api_key_env.clone())
        .unwrap_or_else(|| default_api_key_env(provider).unwrap_or_default());

    if api_key_env.is_empty() {
        return Err(anyhow!("api_key_env not configured for provider {provider}"));
    }

    let api_key = env::var(&api_key_env)
        .with_context(|| format!("missing env var {api_key_env}"))?;

    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "user", "content": prompt}
        ],
        "temperature": temperature,
        "max_tokens": max_output_tokens,
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?
        .error_for_status()?;

    let headers = resp.headers().clone();
    let parsed: OpenAIChatResponse = resp.json().await?;
    let content = parsed
        .choices
        .first()
        .and_then(|c| c.message.content.clone())
        .unwrap_or_default();

    let usage = parsed.usage.map_or(Usage::default(), |u| Usage {
        input_tokens: u.prompt_tokens,
        output_tokens: u.completion_tokens,
        total_tokens: u.total_tokens,
        cost_usd: None,
    });

    let header_cost = headers
        .get("x-litellm-response-cost")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<f64>().ok());

    Ok(CallResult {
        content,
        usage,
        response_id: parsed.id,
        header_cost,
    })
}

async fn fetch_openrouter_cost(config: &Config, response_id: &str) -> Result<Option<f64>> {
    let provider_cfg = config.providers.get("openrouter");
    let base_url = provider_cfg
        .and_then(|p| p.base_url.clone())
        .or_else(|| default_base_url("openrouter"))
        .ok_or_else(|| anyhow!("base_url not found for openrouter"))?;

    let api_key_env = provider_cfg
        .and_then(|p| p.api_key_env.clone())
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

    let client = reqwest::Client::new();
    let resp = client
        .get(url)
        .bearer_auth(api_key)
        .send()
        .await?
        .error_for_status()?;

    let payload: Value = resp.json().await?;
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

fn default_base_url(provider: &str) -> Option<String> {
    match provider {
        "openrouter" => Some("https://openrouter.ai/api/v1".to_string()),
        "openai" => Some("https://api.openai.com/v1".to_string()),
        _ => None,
    }
}

fn default_api_key_env(provider: &str) -> Option<String> {
    match provider {
        "openrouter" => Some("OPENROUTER_API_KEY".to_string()),
        "openai" => Some("OPENAI_API_KEY".to_string()),
        "litellm" => Some("LITELLM_API_KEY".to_string()),
        _ => None,
    }
}
