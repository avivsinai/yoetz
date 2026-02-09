use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use std::fs;
use std::path::PathBuf;

use crate::providers::{gemini, openai, resolve_provider_auth};
use crate::{
    build_model_spec, maybe_write_output, normalize_model_name, parse_media_inputs, resolve_prompt,
    usage_from_litellm, AppContext, GenerateArgs, GenerateCommand, GenerateImageArgs,
    GenerateVideoArgs,
};
use litellm_rust::{ImageEditRequest, ImageInputData, ImageRequest};
use yoetz_core::media::{MediaSource, MediaType};
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};
use yoetz_core::session::{create_session_dir, write_json as write_json_file};
use yoetz_core::types::{ArtifactPaths, MediaGenerationResult, Usage};

pub(crate) async fn handle_generate(
    ctx: &AppContext,
    args: GenerateArgs,
    format: OutputFormat,
) -> Result<()> {
    match args.command {
        GenerateCommand::Image(args) => handle_generate_image(ctx, args, format).await,
        GenerateCommand::Video(args) => handle_generate_video(ctx, args, format).await,
    }
}

async fn handle_generate_image(
    ctx: &AppContext,
    args: GenerateImageArgs,
    format: OutputFormat,
) -> Result<()> {
    let prompt = resolve_prompt(args.prompt, args.prompt_file)?;
    let config = &ctx.config;

    let provider = args
        .provider
        .clone()
        .or(config.defaults.provider.clone())
        .ok_or_else(|| anyhow!("provider is required"))?;
    let model = normalize_model_name(
        &args
            .model
            .clone()
            .or(config.defaults.model.clone())
            .ok_or_else(|| anyhow!("model is required"))?,
    );

    let images = parse_media_inputs(&args.image, &args.image_mime, MediaType::Image)?;

    let session = create_session_dir()?;
    let media_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| session.path.join("media"));
    fs::create_dir_all(&media_dir).with_context(|| format!("create {}", media_dir.display()))?;

    let artifacts = ArtifactPaths {
        session_dir: session.path.to_string_lossy().to_string(),
        media_dir: Some(media_dir.to_string_lossy().to_string()),
        ..Default::default()
    };

    let (outputs, usage) = if args.dry_run {
        (Vec::new(), Usage::default())
    } else if !images.is_empty() {
        match provider.as_str() {
            "openai" => {
                let auth = resolve_provider_auth(config, &provider)?;
                let result = openai::generate_images(
                    &ctx.client,
                    &auth,
                    &prompt,
                    &model,
                    &images,
                    args.size.as_deref(),
                    args.quality.as_deref(),
                    args.background.as_deref(),
                    args.n,
                    &media_dir,
                )
                .await?;
                (result.outputs, result.usage)
            }
            "gemini" => {
                let model_spec = build_model_spec(Some(&provider), &model)?;
                let input_images = images
                    .iter()
                    .map(media_input_to_image_input_data)
                    .collect::<Result<Vec<_>>>()?;
                let resp = ctx
                    .litellm
                    .image_editing(ImageEditRequest {
                        model: model_spec,
                        prompt: prompt.clone(),
                        images: input_images,
                        n: Some(args.n as u32),
                        size: args.size.clone(),
                    })
                    .await?;
                let outputs =
                    crate::save_image_outputs(&ctx.client, resp.images, &media_dir, &model).await?;
                (outputs, usage_from_litellm(resp.usage))
            }
            _ => {
                return Err(anyhow!(
                    "provider {provider} does not support image edits yet"
                ))
            }
        }
    } else {
        let model_spec = build_model_spec(Some(&provider), &model)?;
        let resp = ctx
            .litellm
            .image_generation(ImageRequest {
                model: model_spec,
                prompt: prompt.clone(),
                n: Some(args.n as u32),
                size: args.size.clone(),
                quality: args.quality.clone(),
                background: args.background.clone(),
            })
            .await?;
        let outputs =
            crate::save_image_outputs(&ctx.client, resp.images, &media_dir, &model).await?;
        (outputs, usage_from_litellm(resp.usage))
    };

    let mut result = MediaGenerationResult {
        id: session.id,
        provider: Some(provider),
        model: Some(model),
        prompt,
        usage,
        artifacts: artifacts.clone(),
        outputs,
    };

    let response_json = PathBuf::from(&artifacts.session_dir).join("response.json");
    result.artifacts.response_json = Some(response_json.to_string_lossy().to_string());
    write_json_file(&response_json, &result)?;

    maybe_write_output(ctx, &result)?;

    match format {
        OutputFormat::Json => write_json(&result),
        OutputFormat::Jsonl => write_jsonl("generate.image", &result),
        OutputFormat::Text | OutputFormat::Markdown => {
            for output in &result.outputs {
                println!("{}", output.path.display());
            }
            Ok(())
        }
    }
}

fn media_input_to_image_input_data(
    media: &yoetz_core::media::MediaInput,
) -> Result<ImageInputData> {
    match &media.source {
        MediaSource::Base64 { data, mime } => Ok(ImageInputData {
            b64_json: Some(data.clone()),
            url: None,
            mime_type: Some(mime.clone()),
        }),
        MediaSource::Url(url) => Ok(ImageInputData {
            b64_json: None,
            url: Some(url.clone()),
            mime_type: Some(media.mime_type.clone()),
        }),
        MediaSource::File(path) => {
            let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
            let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
            Ok(ImageInputData {
                b64_json: Some(b64),
                url: None,
                mime_type: Some(media.mime_type.clone()),
            })
        }
        MediaSource::FileApiId { id, .. } => Ok(ImageInputData {
            b64_json: None,
            url: Some(id.clone()),
            mime_type: Some(media.mime_type.clone()),
        }),
    }
}

async fn handle_generate_video(
    ctx: &AppContext,
    args: GenerateVideoArgs,
    format: OutputFormat,
) -> Result<()> {
    let prompt = resolve_prompt(args.prompt, args.prompt_file)?;
    let config = &ctx.config;

    let provider = args
        .provider
        .clone()
        .or(config.defaults.provider.clone())
        .ok_or_else(|| anyhow!("provider is required"))?;
    let model = normalize_model_name(
        &args
            .model
            .clone()
            .or(config.defaults.model.clone())
            .ok_or_else(|| anyhow!("model is required"))?,
    );

    let images = parse_media_inputs(&args.image, &args.image_mime, MediaType::Image)?;

    let session = create_session_dir()?;
    let media_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| session.path.join("media"));
    fs::create_dir_all(&media_dir).with_context(|| format!("create {}", media_dir.display()))?;

    let artifacts = ArtifactPaths {
        session_dir: session.path.to_string_lossy().to_string(),
        media_dir: Some(media_dir.to_string_lossy().to_string()),
        ..Default::default()
    };

    let output_path = media_dir.join("video.mp4");

    let outputs = if args.dry_run {
        Vec::new()
    } else {
        let output = match provider.as_str() {
            "openai" => {
                let auth = resolve_provider_auth(config, &provider)?;
                openai::generate_video_sora(
                    &ctx.client,
                    &auth,
                    &prompt,
                    &model,
                    args.duration_secs,
                    args.size.as_deref(),
                    images.first(),
                    &output_path,
                )
                .await?
            }
            "gemini" => {
                let auth = resolve_provider_auth(config, &provider)?;
                gemini::generate_video_veo(
                    &ctx.client,
                    &auth,
                    &prompt,
                    &model,
                    &images,
                    args.duration_secs,
                    args.aspect_ratio.as_deref(),
                    args.resolution.as_deref(),
                    args.negative_prompt.as_deref(),
                    &output_path,
                )
                .await?
            }
            _ => {
                return Err(anyhow!(
                    "provider {provider} does not support video generation yet"
                ))
            }
        };
        vec![output]
    };

    let mut result = MediaGenerationResult {
        id: session.id,
        provider: Some(provider),
        model: Some(model),
        prompt,
        usage: Usage::default(),
        artifacts: artifacts.clone(),
        outputs,
    };

    let response_json = PathBuf::from(&artifacts.session_dir).join("response.json");
    result.artifacts.response_json = Some(response_json.to_string_lossy().to_string());
    write_json_file(&response_json, &result)?;

    maybe_write_output(ctx, &result)?;

    match format {
        OutputFormat::Json => write_json(&result),
        OutputFormat::Jsonl => write_jsonl("generate.video", &result),
        OutputFormat::Text | OutputFormat::Markdown => {
            for output in &result.outputs {
                println!("{}", output.path.display());
            }
            Ok(())
        }
    }
}
