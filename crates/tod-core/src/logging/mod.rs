//! Diagnostic logging init and helpers (no call-site facade).

mod prune;

pub use prune::prune_log_dir;
pub use tod_store::LogLevel;

use anyhow::{Context, Result};
use chrono::DateTime;
use chrono::Local;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_rolling_file::{RollingCondition, RollingFileAppender};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::reload;

static WORKER_GUARD: Mutex<Option<WorkerGuard>> = Mutex::new(None);
static FILTER_RELOAD: OnceLock<reload::Handle<EnvFilter, tracing_subscriber::Registry>> =
    OnceLock::new();
static CLI_LEVEL_LOCKED: AtomicBool = AtomicBool::new(false);
static MAX_SIZE_BYTES: AtomicU64 = AtomicU64::new(0);
static LOG_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Active `tod.log` plus this many rolled files (`tod.log.1` …).
const MAX_ROLLED_FILES: usize = 9;
/// Slots used to map total max bytes → per-file rotate size (active + rolled).
const FILE_SLOTS: u64 = (MAX_ROLLED_FILES as u64) + 1;

pub struct InitConfig {
    pub log_dir: PathBuf,
    pub level: LogLevel,
    pub max_size_kb: u64,
    /// When true, settings verbosity changes do not reload the filter this run.
    pub cli_override: bool,
}

/// Size condition that reads the live max-bytes setting (mid-run updates apply).
struct SizeCapCondition;

impl RollingCondition for SizeCapCondition {
    fn should_rollover(&mut self, _now: &DateTime<Local>, current_filesize: u64) -> bool {
        let max = MAX_SIZE_BYTES.load(Ordering::Relaxed).max(1);
        let per_file = (max / FILE_SLOTS).max(1);
        current_filesize >= per_file
    }
}

/// Initialize the global tracing subscriber with an NDJSON file sink under `log_dir`.
pub fn init(config: InitConfig) -> Result<()> {
    std::fs::create_dir_all(&config.log_dir).with_context(|| {
        format!(
            "failed to create log directory {}",
            config.log_dir.display()
        )
    })?;

    let max_bytes = config.max_size_kb.saturating_mul(1024);
    MAX_SIZE_BYTES.store(max_bytes, Ordering::Relaxed);
    CLI_LEVEL_LOCKED.store(config.cli_override, Ordering::Relaxed);
    *LOG_DIR.lock().expect("log dir lock") = Some(config.log_dir.clone());

    prune_log_dir(&config.log_dir, max_bytes)?;

    let log_path = config.log_dir.join("tod.log");
    let file_appender = RollingFileAppender::new(&log_path, SizeCapCondition, MAX_ROLLED_FILES)
        .with_context(|| {
            format!(
                "failed to initialize log file appender at {}",
                log_path.display()
            )
        })?;
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    *WORKER_GUARD.lock().expect("worker guard lock") = Some(guard);

    let filter = EnvFilter::new(config.level.as_filter_directive());
    let (filter_layer, reload_handle) = reload::Layer::new(filter);
    let _ = FILTER_RELOAD.set(reload_handle);

    let fmt_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_ansi(false)
        .with_writer(non_blocking)
        .with_current_span(false)
        .with_span_list(false);

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .try_init()
        .context("failed to install tracing subscriber")?;

    tracing::info!(
        event = "lifecycle",
        action = "process_start",
        "tod process started"
    );

    Ok(())
}

pub fn cli_level_locked() -> bool {
    CLI_LEVEL_LOCKED.load(Ordering::Relaxed)
}

/// Reload the minimum emit level when settings change mid-run (no-op if CLI locked).
pub fn reload_level(level: LogLevel) -> Result<()> {
    if cli_level_locked() {
        return Ok(());
    }
    let handle = FILTER_RELOAD
        .get()
        .context("logging filter reload handle not initialized")?;
    handle
        .reload(EnvFilter::new(level.as_filter_directive()))
        .context("failed to reload log level filter")?;
    Ok(())
}

/// Update the on-disk size cap (bytes = kb × 1024) and prune immediately.
pub fn set_max_size_kb(kb: u64) -> Result<()> {
    let max_bytes = kb.saturating_mul(1024);
    MAX_SIZE_BYTES.store(max_bytes, Ordering::Relaxed);
    if let Some(dir) = LOG_DIR.lock().expect("log dir lock").clone() {
        prune_log_dir(&dir, max_bytes)?;
    }
    Ok(())
}

pub fn absolute_log_dir(log_dir: &Path) -> PathBuf {
    std::path::absolute(log_dir).unwrap_or_else(|_| log_dir.to_path_buf())
}

/// Effective minimum level: CLI if present, else settings, else info.
pub fn resolve_level(cli: Option<LogLevel>, settings: LogLevel) -> LogLevel {
    cli.unwrap_or(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_cli_over_settings() {
        assert_eq!(
            resolve_level(Some(LogLevel::Error), LogLevel::Debug),
            LogLevel::Error
        );
        assert_eq!(resolve_level(None, LogLevel::Debug), LogLevel::Debug);
        assert_eq!(resolve_level(None, LogLevel::Info), LogLevel::Info);
    }
}
