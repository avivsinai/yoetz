use anyhow::{anyhow, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use tempfile::NamedTempFile;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetLedger {
    pub date: String,
    pub spent_usd: f64,
}

impl Default for BudgetLedger {
    fn default() -> Self {
        Self {
            date: today_utc(),
            spent_usd: 0.0,
        }
    }
}

pub fn budget_path() -> PathBuf {
    if let Ok(path) = env::var("YOETZ_BUDGET_PATH") {
        return PathBuf::from(path);
    }
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join(".yoetz/budget.json");
    }
    PathBuf::from(".yoetz/budget.json")
}

fn budget_lock_path() -> PathBuf {
    let mut path = budget_path();
    let lock_name = match path.file_name() {
        Some(name) => format!("{}.lock", name.to_string_lossy()),
        None => "budget.json.lock".to_string(),
    };
    path.set_file_name(lock_name);
    path
}

fn acquire_budget_lock() -> Result<std::fs::File> {
    let lock_path = budget_lock_path();
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(&lock_path)?;
    file.lock_exclusive()
        .with_context(|| format!("lock budget {}", lock_path.display()))?;
    Ok(file)
}

pub fn load_ledger() -> Result<BudgetLedger> {
    let _lock = acquire_budget_lock()?;
    let path = budget_path();
    if !path.exists() {
        return Ok(BudgetLedger::default());
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("read budget {}", path.display()))?;
    let mut ledger: BudgetLedger = serde_json::from_str(&content)?;
    let today = today_utc();
    if ledger.date != today {
        ledger.date = today;
        ledger.spent_usd = 0.0;
    }
    Ok(ledger)
}

pub fn save_ledger(ledger: &BudgetLedger) -> Result<()> {
    let _lock = acquire_budget_lock()?;
    let path = budget_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(ledger)?;
    let mut tmp =
        NamedTempFile::new_in(path.parent().unwrap_or_else(|| std::path::Path::new(".")))?;
    tmp.write_all(data.as_bytes())?;
    tmp.persist(&path)
        .map_err(|e| anyhow!("write budget {}: {}", path.display(), e))?;
    Ok(())
}

pub fn ensure_budget(
    estimate_usd: Option<f64>,
    max_cost_usd: Option<f64>,
    daily_budget_usd: Option<f64>,
) -> Result<BudgetLedger> {
    if let Some(max) = max_cost_usd {
        let Some(estimate) = estimate_usd else {
            return Err(anyhow!(
                "cost estimate unavailable; cannot enforce max-cost"
            ));
        };
        if estimate > max {
            return Err(anyhow!(
                "estimated cost ${estimate:.6} exceeds max ${max:.6}"
            ));
        }
    }

    let ledger = load_ledger()?;
    if let Some(limit) = daily_budget_usd {
        let Some(estimate) = estimate_usd else {
            return Err(anyhow!(
                "cost estimate unavailable; cannot enforce daily budget"
            ));
        };
        if ledger.spent_usd + estimate > limit {
            return Err(anyhow!(
                "daily budget exceeded: ${:.6} + ${:.6} > ${:.6}",
                ledger.spent_usd,
                estimate,
                limit
            ));
        }
    }

    Ok(ledger)
}

pub fn record_spend(mut ledger: BudgetLedger, spend_usd: f64) -> Result<()> {
    ledger.spent_usd += spend_usd;
    save_ledger(&ledger)
}

fn today_utc() -> String {
    OffsetDateTime::now_utc().date().to_string()
}

#[allow(dead_code)]
fn timestamp_utc() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}
