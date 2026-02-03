use anyhow::{anyhow, Result};
use serde::Serialize;
use std::io::{self, Write};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Json,
    Jsonl,
    Text,
    Markdown,
}

impl FromStr for OutputFormat {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "json" => Ok(OutputFormat::Json),
            "jsonl" => Ok(OutputFormat::Jsonl),
            "text" => Ok(OutputFormat::Text),
            "markdown" | "md" => Ok(OutputFormat::Markdown),
            _ => Err(anyhow!("unknown format: {s}")),
        }
    }
}

pub fn write_json<T: Serialize>(value: &T) -> Result<()> {
    let data = serde_json::to_string_pretty(value)?;
    println!("{data}");
    Ok(())
}

pub fn write_jsonl_event<T: Serialize>(event: &T) -> Result<()> {
    let mut stdout = io::stdout().lock();
    let line = serde_json::to_string(event)?;
    stdout.write_all(line.as_bytes())?;
    stdout.write_all(b"\n")?;
    Ok(())
}

#[derive(Serialize)]
struct JsonlEvent<'a, T> {
    #[serde(rename = "type")]
    kind: &'a str,
    data: &'a T,
}

pub fn write_jsonl<T: Serialize>(kind: &str, data: &T) -> Result<()> {
    let event = JsonlEvent { kind, data };
    write_jsonl_event(&event)
}
