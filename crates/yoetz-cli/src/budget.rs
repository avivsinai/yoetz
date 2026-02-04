use anyhow::{anyhow, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use tempfile::NamedTempFile;
use time::{format_description::well_known::Rfc3339, Duration, OffsetDateTime};
use yoetz_core::paths::home_dir;

const RESERVATION_TTL_SECS: i64 = 2 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetLedger {
    pub date: String,
    pub spent_usd: f64,
    #[serde(default)]
    pub reservations: Vec<BudgetReservationEntry>,
}

impl Default for BudgetLedger {
    fn default() -> Self {
        Self {
            date: today_utc(),
            spent_usd: 0.0,
            reservations: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetReservationEntry {
    pub id: String,
    pub reserved_usd: f64,
    pub created_at: String,
}

#[derive(Debug)]
pub struct BudgetReservation {
    id: String,
    active: bool,
}

impl BudgetReservation {
    pub fn commit(mut self, spend_usd: f64) -> Result<()> {
        record_spend_with_reservation(&self.id, spend_usd)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for BudgetReservation {
    fn drop(&mut self) {
        if self.active {
            let _ = release_reservation(&self.id);
        }
    }
}

/// A guard that holds the budget lock and provides atomic operations.
///
/// The lock is held for the lifetime of this guard, preventing race conditions
/// between reading and writing the budget file.
#[derive(Debug)]
pub struct BudgetGuard {
    _lock_file: File,
    ledger: BudgetLedger,
}

impl BudgetGuard {
    /// Acquire the budget lock and load the current ledger state.
    ///
    /// The lock is held until this guard is dropped.
    pub fn acquire() -> Result<Self> {
        let lock_file = acquire_budget_lock()?;
        let ledger = load_ledger_unlocked()?;
        Ok(Self {
            _lock_file: lock_file,
            ledger,
        })
    }

    /// Get a reference to the current ledger state.
    #[allow(dead_code)]
    pub fn ledger(&self) -> &BudgetLedger {
        &self.ledger
    }

    /// Get the current spent amount in USD.
    /// Add spend to the ledger and persist atomically.
    ///
    /// This operation is atomic because we hold the lock.
    pub fn record_spend(&mut self, spend_usd: f64) -> Result<()> {
        self.ledger.spent_usd += spend_usd;
        save_ledger_unlocked(&self.ledger)
    }

    /// Consume the guard and return the ledger for further use.
    ///
    /// Note: The lock is released when this is called.
    #[allow(dead_code)]
    pub fn into_ledger(self) -> BudgetLedger {
        self.ledger
    }
}

pub fn budget_path() -> PathBuf {
    if let Ok(path) = env::var("YOETZ_BUDGET_PATH") {
        return PathBuf::from(path);
    }
    if let Some(home) = home_dir() {
        return home.join(".yoetz/budget.json");
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

fn acquire_budget_lock() -> Result<File> {
    // File locks are advisory; this is a cooperative lock between yoetz processes.
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

/// Load the ledger without acquiring the lock.
///
/// This should only be called when the lock is already held.
fn load_ledger_unlocked() -> Result<BudgetLedger> {
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
        ledger.reservations.clear();
    }
    prune_reservations(&mut ledger);
    Ok(ledger)
}

/// Save the ledger without acquiring the lock.
///
/// This should only be called when the lock is already held.
fn save_ledger_unlocked(ledger: &BudgetLedger) -> Result<()> {
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

/// Load the budget ledger (acquires and releases lock).
///
/// For operations that need to read and then write, use `BudgetGuard::acquire()`
/// instead to hold the lock across both operations.
#[allow(dead_code)]
pub fn load_ledger() -> Result<BudgetLedger> {
    let guard = BudgetGuard::acquire()?;
    Ok(guard.into_ledger())
}

/// Save the budget ledger (acquires and releases lock).
///
/// For operations that need to read and then write, use `BudgetGuard::acquire()`
/// instead to hold the lock across both operations.
#[allow(dead_code)]
pub fn save_ledger(ledger: &BudgetLedger) -> Result<()> {
    let _lock = acquire_budget_lock()?;
    save_ledger_unlocked(ledger)
}

/// Validate budget constraints and optionally reserve estimated spend.
///
/// This function:
/// 1. Validates the estimated cost against max_cost_usd
/// 2. If a daily budget is set, reserves the estimate under a short-lived reservation
/// 3. Returns a reservation that should be committed with the actual spend
///
/// Reservations are released on drop to avoid blocking concurrent runs.
pub fn ensure_budget(
    estimate_usd: Option<f64>,
    max_cost_usd: Option<f64>,
    daily_budget_usd: Option<f64>,
) -> Result<Option<BudgetReservation>> {
    // Validate max cost before acquiring lock
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

    let Some(limit) = daily_budget_usd else {
        return Ok(None);
    };

    let Some(estimate) = estimate_usd else {
        return Err(anyhow!(
            "cost estimate unavailable; cannot enforce daily budget"
        ));
    };

    let _lock = acquire_budget_lock()?;
    let mut ledger = load_ledger_unlocked()?;
    let reserved = ledger
        .reservations
        .iter()
        .map(|r| r.reserved_usd)
        .sum::<f64>();
    if ledger.spent_usd + reserved + estimate > limit {
        return Err(anyhow!(
            "daily budget exceeded: ${:.6} + ${:.6} + ${:.6} > ${:.6}",
            ledger.spent_usd,
            reserved,
            estimate,
            limit
        ));
    }

    let reservation = add_reservation(&mut ledger, estimate)?;
    save_ledger_unlocked(&ledger)?;

    Ok(Some(reservation))
}

/// Record spend in an existing budget guard.
///
/// This is the preferred way to record spend as it maintains the lock
/// from validation through recording.
#[allow(dead_code)]
pub fn record_spend(mut guard: BudgetGuard, spend_usd: f64) -> Result<()> {
    guard.record_spend(spend_usd)
}

/// Record spend without a guard (for backwards compatibility).
///
/// Note: This acquires a new lock, so there's a small window where
/// another process could have modified the budget. Prefer using
/// `ensure_budget` and committing its reservation with the actual spend.
#[allow(dead_code)]
pub fn record_spend_standalone(spend_usd: f64) -> Result<()> {
    let mut guard = BudgetGuard::acquire()?;
    guard.record_spend(spend_usd)
}

fn today_utc() -> String {
    OffsetDateTime::now_utc().date().to_string()
}

fn add_reservation(ledger: &mut BudgetLedger, estimate_usd: f64) -> Result<BudgetReservation> {
    let id = reservation_id();
    let created_at = timestamp_utc();
    ledger.reservations.push(BudgetReservationEntry {
        id: id.clone(),
        reserved_usd: estimate_usd,
        created_at,
    });
    Ok(BudgetReservation { id, active: true })
}

fn release_reservation(id: &str) -> Result<()> {
    let _lock = acquire_budget_lock()?;
    let mut ledger = load_ledger_unlocked()?;
    let before = ledger.reservations.len();
    ledger.reservations.retain(|r| r.id != id);
    if ledger.reservations.len() != before {
        save_ledger_unlocked(&ledger)?;
    }
    Ok(())
}

fn record_spend_with_reservation(id: &str, spend_usd: f64) -> Result<()> {
    let _lock = acquire_budget_lock()?;
    let mut ledger = load_ledger_unlocked()?;
    ledger.reservations.retain(|r| r.id != id);
    ledger.spent_usd += spend_usd;
    save_ledger_unlocked(&ledger)
}

fn prune_reservations(ledger: &mut BudgetLedger) {
    let cutoff = OffsetDateTime::now_utc() - Duration::seconds(RESERVATION_TTL_SECS);
    ledger.reservations.retain(|r| {
        OffsetDateTime::parse(&r.created_at, &Rfc3339)
            .map(|ts| ts >= cutoff)
            .unwrap_or(false)
    });
}

fn reservation_id() -> String {
    let pid = std::process::id();
    let nanos = OffsetDateTime::now_utc().unix_timestamp_nanos();
    format!("{pid}-{nanos}")
}

#[allow(dead_code)]
fn timestamp_utc() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_budget_path() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("yoetz_budget_test_{nanos}"))
    }

    #[test]
    #[serial]
    fn budget_guard_atomic_operations() {
        let test_dir = temp_budget_path();
        fs::create_dir_all(&test_dir).unwrap();
        let budget_file = test_dir.join("budget.json");
        env::set_var("YOETZ_BUDGET_PATH", &budget_file);

        // Initial state
        {
            let guard = BudgetGuard::acquire().unwrap();
            assert_eq!(guard.ledger().spent_usd, 0.0);
        }

        // Record spend
        {
            let mut guard = BudgetGuard::acquire().unwrap();
            guard.record_spend(1.50).unwrap();
        }

        // Verify persisted
        {
            let guard = BudgetGuard::acquire().unwrap();
            assert!((guard.ledger().spent_usd - 1.50).abs() < f64::EPSILON);
        }

        // Multiple spends
        {
            let mut guard = BudgetGuard::acquire().unwrap();
            guard.record_spend(0.25).unwrap();
        }

        {
            let guard = BudgetGuard::acquire().unwrap();
            assert!((guard.ledger().spent_usd - 1.75).abs() < f64::EPSILON);
        }

        // Cleanup
        env::remove_var("YOETZ_BUDGET_PATH");
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    #[serial]
    fn ensure_budget_validates_limits() {
        let test_dir = temp_budget_path();
        fs::create_dir_all(&test_dir).unwrap();
        let budget_file = test_dir.join("budget.json");
        env::set_var("YOETZ_BUDGET_PATH", &budget_file);

        // Max cost validation
        let result = ensure_budget(Some(10.0), Some(5.0), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("exceeds max"));

        // Daily budget validation after some spend
        {
            let mut guard = BudgetGuard::acquire().unwrap();
            guard.record_spend(8.0).unwrap();
        }

        let result = ensure_budget(Some(5.0), None, Some(10.0));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("daily budget exceeded"));

        let reservation = ensure_budget(Some(1.0), None, Some(10.0)).unwrap().unwrap();
        reservation.commit(1.0).unwrap();

        let ledger = load_ledger().unwrap();
        assert!((ledger.spent_usd - 9.0).abs() < f64::EPSILON);
        assert!(ledger.reservations.is_empty());

        // Cleanup
        env::remove_var("YOETZ_BUDGET_PATH");
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    #[serial]
    fn budget_reservation_drop_releases() {
        let test_dir = temp_budget_path();
        fs::create_dir_all(&test_dir).unwrap();
        let budget_file = test_dir.join("budget.json");
        env::set_var("YOETZ_BUDGET_PATH", &budget_file);

        let reservation = ensure_budget(Some(2.0), None, Some(10.0)).unwrap().unwrap();
        drop(reservation);

        let ledger = load_ledger().unwrap();
        assert!(ledger.reservations.is_empty());

        env::remove_var("YOETZ_BUDGET_PATH");
        let _ = fs::remove_dir_all(&test_dir);
    }
}
