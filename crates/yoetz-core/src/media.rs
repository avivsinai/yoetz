use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose, Engine as _};
use mime_guess::MimeGuess;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MediaType {
    Image,
    Video,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MediaSource {
    File(PathBuf),
    Url(String),
    Base64 { data: String, mime: String },
    FileApiId { id: String, provider: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInput {
    pub source: MediaSource,
    pub media_type: MediaType,
    pub mime_type: String,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaOutput {
    pub media_type: MediaType,
    pub path: PathBuf,
    pub url: Option<String>,
    pub metadata: MediaMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaMetadata {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_secs: Option<f32>,
    pub model: String,
    pub revised_prompt: Option<String>,
}

impl MediaInput {
    pub fn from_path(path: &Path) -> Result<Self> {
        let mime = guess_mime(path)?;
        let media_type = media_type_from_mime(&mime)?;
        let size_bytes = fs::metadata(path).ok().map(|m| m.len());
        Ok(Self {
            source: MediaSource::File(path.to_path_buf()),
            media_type,
            mime_type: mime,
            size_bytes,
        })
    }

    pub fn from_url(url: &str, mime_type: Option<&str>) -> Result<Self> {
        let mime = mime_type
            .map(|m| m.to_string())
            .unwrap_or_else(|| guess_mime_from_url(url));
        let media_type = media_type_from_mime(&mime)?;
        Ok(Self {
            source: MediaSource::Url(url.to_string()),
            media_type,
            mime_type: mime,
            size_bytes: None,
        })
    }

    pub fn as_data_url(&self) -> Result<String> {
        let (data, mime) = match &self.source {
            MediaSource::Base64 { data, mime } => (data.clone(), mime.clone()),
            MediaSource::File(path) => {
                let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
                (general_purpose::STANDARD.encode(bytes), self.mime_type.clone())
            }
            MediaSource::Url(url) => return Ok(url.clone()),
            MediaSource::FileApiId { id, .. } => {
                return Err(anyhow!("file API id cannot be converted to data url: {id}"));
            }
        };
        Ok(format!("data:{};base64,{}", mime, data))
    }

    pub fn read_bytes(&self) -> Result<Vec<u8>> {
        match &self.source {
            MediaSource::File(path) => {
                fs::read(path).with_context(|| format!("read {}", path.display()))
            }
            MediaSource::Base64 { data, .. } => {
                general_purpose::STANDARD
                    .decode(data)
                    .context("decode base64 media")
            }
            MediaSource::Url(url) => Err(anyhow!("cannot read bytes from url: {url}")),
            MediaSource::FileApiId { id, .. } => {
                Err(anyhow!("cannot read bytes from file API id: {id}"))
            }
        }
    }
}

fn guess_mime(path: &Path) -> Result<String> {
    let mime = MimeGuess::from_path(path).first_or_octet_stream();
    Ok(mime.essence_str().to_string())
}

fn guess_mime_from_url(url: &str) -> String {
    let mime = MimeGuess::from_path(url).first_or_octet_stream();
    mime.essence_str().to_string()
}

fn media_type_from_mime(mime: &str) -> Result<MediaType> {
    if mime.starts_with("image/") {
        Ok(MediaType::Image)
    } else if mime.starts_with("video/") {
        Ok(MediaType::Video)
    } else {
        Err(anyhow!("unsupported media mime type: {mime}"))
    }
}
