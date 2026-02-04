use anyhow::{anyhow, Result};
use reqwest::{header::HeaderMap, RequestBuilder};
use serde::de::DeserializeOwned;

pub async fn send_json<T: DeserializeOwned>(req: RequestBuilder) -> Result<(T, HeaderMap)> {
    let resp = req.send().await?;
    let status = resp.status();
    let headers = resp.headers().clone();
    if !status.is_success() {
        let text = resp.text().await?;
        let trimmed = text.lines().take(20).collect::<Vec<_>>().join("\n");
        return Err(anyhow!("http {}: {}", status.as_u16(), trimmed));
    }
    let parsed = resp.json().await?;
    Ok((parsed, headers))
}
