use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose, Engine as _};
use mime_guess::MimeGuess;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Image or video media type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

/// A media file or URL to send to an LLM provider.
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
        Self::from_path_with_mime(path, None)
    }

    pub fn from_path_with_mime(path: &Path, mime_override: Option<&str>) -> Result<Self> {
        let mime = if let Some(mime) = mime_override {
            mime.to_string()
        } else {
            guess_mime(path)?
        };
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

    pub fn from_url_with_type(
        url: &str,
        expected_type: MediaType,
        mime_type: Option<&str>,
    ) -> Result<Self> {
        let mime = mime_type
            .map(|m| m.to_string())
            .unwrap_or_else(|| guess_mime_from_url(url));
        match media_type_from_mime(&mime) {
            Ok(detected) if detected != expected_type => {
                return Err(anyhow!(
                    "expected {expected_type:?} url but got mime {mime}; pass an explicit mime override to force it"
                ));
            }
            Ok(_) => {}
            Err(_) => {}
        }
        Ok(Self {
            source: MediaSource::Url(url.to_string()),
            media_type: expected_type,
            mime_type: mime,
            size_bytes: None,
        })
    }

    pub fn as_data_url(&self) -> Result<String> {
        let (data, mime) = match &self.source {
            MediaSource::Base64 { data, mime } => (data.clone(), mime.clone()),
            MediaSource::File(path) => {
                let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
                (
                    general_purpose::STANDARD.encode(bytes),
                    self.mime_type.clone(),
                )
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
            MediaSource::Base64 { data, .. } => general_purpose::STANDARD
                .decode(data)
                .context("decode base64 media"),
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
    let trimmed = url
        .split_once('?')
        .map(|(base, _)| base)
        .unwrap_or(url)
        .split_once('#')
        .map(|(base, _)| base)
        .unwrap_or(url);
    let mime = MimeGuess::from_path(trimmed).first_or_octet_stream();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(ext: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("yoetz_media_test_{nanos}.{ext}"))
    }

    #[test]
    fn media_input_from_path_image() {
        let path = temp_path("png");
        fs::write(&path, [0u8, 1, 2, 3]).unwrap();
        let input = MediaInput::from_path(&path).unwrap();
        assert_eq!(input.media_type, MediaType::Image);
        assert!(input.mime_type.starts_with("image/"));
        let data_url = input.as_data_url().unwrap();
        assert!(data_url.starts_with("data:image/"));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn media_input_from_path_video() {
        let path = temp_path("mp4");
        fs::write(&path, [0u8, 1, 2, 3]).unwrap();
        let input = MediaInput::from_path(&path).unwrap();
        assert_eq!(input.media_type, MediaType::Video);
        assert!(input.mime_type.starts_with("video/"));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn media_input_from_path_with_mime_override() {
        let path = temp_path("bin");
        fs::write(&path, [0u8, 1, 2, 3]).unwrap();
        let input = MediaInput::from_path_with_mime(&path, Some("image/png")).unwrap();
        assert_eq!(input.media_type, MediaType::Image);
        assert_eq!(input.mime_type, "image/png");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn media_input_from_url_with_type_unknown_mime() {
        let input =
            MediaInput::from_url_with_type("https://example.com/blob", MediaType::Image, None)
                .unwrap();
        assert_eq!(input.media_type, MediaType::Image);
        assert_eq!(input.mime_type, "application/octet-stream");
    }
}
