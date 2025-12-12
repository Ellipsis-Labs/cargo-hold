//! Heave (garbage collection) command and helpers.

use std::path::Path;

use crate::error::Result;
use crate::gc::{self, Gc};
use crate::metadata::{load_metadata, save_metadata};
use crate::state::{CapTrace, GcMetrics, StateMetadata};

pub(crate) const GC_METRICS_WINDOW: usize = 20;
pub(crate) const MIN_HEADROOM_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB safety cushion
pub(crate) const MIN_STEADY_HEADROOM_BYTES: u64 = 256 * 1024 * 1024; // 256 MiB cushion once a cap exists
pub(crate) const MAX_GROWTH_FACTOR_PER_RUN_PCT: u64 = 10; // limit upward drift to +10% per run
pub(crate) const MAX_SHRINK_FACTOR_PER_RUN_PCT: u64 = 10; // limit downward drift to -10% per run
pub(crate) const GROWTH_DEADBAND_PCT: u64 = 5; // tolerate small oscillations without moving the cap

pub struct Heave<'a> {
    target_dir: &'a Path,
    max_target_size: Option<&'a str>,
    auto_max_target_size: bool,
    dry_run: bool,
    debug: bool,
    preserve_cargo_binaries: &'a [String],
    age_threshold_days: u32,
    verbose: u8,
    metadata_path: Option<&'a Path>,
    quiet: bool,
}

#[derive(Default)]
pub struct HeaveBuilder<'a> {
    target_dir: Option<&'a Path>,
    max_target_size: Option<&'a str>,
    auto_max_target_size: bool,
    dry_run: bool,
    debug: bool,
    preserve_cargo_binaries: &'a [String],
    age_threshold_days: u32,
    verbose: u8,
    metadata_path: Option<&'a Path>,
    quiet: bool,
}

impl<'a> HeaveBuilder<'a> {
    pub fn new() -> Self {
        Self {
            target_dir: None,
            max_target_size: None,
            auto_max_target_size: true,
            dry_run: false,
            debug: false,
            preserve_cargo_binaries: &[],
            age_threshold_days: 7,
            verbose: 0,
            metadata_path: None,
            quiet: false,
        }
    }

    pub fn target_dir(mut self, path: &'a Path) -> Self {
        self.target_dir = Some(path);
        self
    }

    pub fn max_target_size(mut self, size: Option<&'a str>) -> Self {
        self.max_target_size = size;
        self
    }

    pub fn auto_max_target_size(mut self, enabled: bool) -> Self {
        self.auto_max_target_size = enabled;
        self
    }

    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    pub fn debug(mut self, debug: bool) -> Self {
        self.debug = debug;
        self
    }

    pub fn preserve_cargo_binaries(mut self, binaries: &'a [String]) -> Self {
        self.preserve_cargo_binaries = binaries;
        self
    }

    pub fn age_threshold_days(mut self, days: u32) -> Self {
        self.age_threshold_days = days;
        self
    }

    pub fn verbose(mut self, verbose: u8) -> Self {
        self.verbose = verbose;
        self
    }

    pub fn metadata_path(mut self, path: &'a Path) -> Self {
        self.metadata_path = Some(path);
        self
    }

    pub fn quiet(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
        self
    }

    pub fn build(self) -> Heave<'a> {
        Heave {
            target_dir: self.target_dir.unwrap(),
            max_target_size: self.max_target_size,
            auto_max_target_size: self.auto_max_target_size,
            dry_run: self.dry_run,
            debug: self.debug,
            preserve_cargo_binaries: self.preserve_cargo_binaries,
            age_threshold_days: self.age_threshold_days,
            verbose: self.verbose,
            metadata_path: self.metadata_path,
            quiet: self.quiet,
        }
    }
}

impl<'a> Heave<'a> {
    pub fn builder<'b>() -> HeaveBuilder<'b> {
        HeaveBuilder::new()
    }

    /// Execute the heave command (garbage collection)
    pub fn heave(self) -> Result<()> {
        if !self.quiet && self.verbose > 0 {
            eprintln!("Heave ho! Starting garbage collection...");
        }

        let mut max_size = if let Some(size_str) = self.max_target_size {
            Some(gc::parse_size(size_str)?)
        } else {
            None
        };

        let loaded_metadata = if let Some(path) = self.metadata_path {
            match load_metadata(path) {
                Ok(metadata) => Some(metadata),
                Err(err) => {
                    if !self.quiet {
                        eprintln!(
                            "Warning: failed to load metadata for GC metrics ({}). Continuing \
                             with defaults.",
                            err
                        );
                    }
                    None
                }
            }
        } else {
            None
        };

        let current_size = gc::calculate_directory_size(self.target_dir)
            .ok()
            .filter(|size| *size > 0);

        let last_gc_mtime_nanos = loaded_metadata.as_ref().and_then(|m| m.last_gc_mtime_nanos);

        if !self.quiet
            && let Some(mtime) = last_gc_mtime_nanos
        {
            let mtime_secs = (mtime / 1_000_000_000) as u64;
            eprintln!(
                "Using previous build timestamp for artifact preservation ({}s ago)",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs().saturating_sub(mtime_secs))
                    .unwrap_or(0)
            );
        }

        let mut auto_cap_used = false;
        let mut cap_trace: Option<CapTrace> = None;
        if max_size.is_none()
            && self.auto_max_target_size
            && let Some(metadata) = loaded_metadata.as_ref()
            && let Some((suggested, trace)) =
                suggest_max_target_size(&metadata.gc_metrics, current_size)
        {
            max_size = Some(suggested);
            auto_cap_used = true;
            cap_trace = Some(trace.clone());
            if !self.quiet {
                if let Some(trace) = cap_trace.as_ref() {
                    // Always log a concise summary (even without verbose) so CI logs show why the
                    // cap moved.
                    eprintln!(
                        "Auto-selected max target size: {} (baseline {}, headroom {}, growth p90 \
                         {}%, clamp {})",
                        gc::format_size(suggested),
                        gc::format_size(trace.baseline),
                        gc::format_size(trace.growth_budget),
                        trace.observed_growth_pct,
                        trace.clamp_reason
                    );
                }
            }
        }

        let mut builder = Gc::builder()
            .target_dir(self.target_dir.to_path_buf())
            .dry_run(self.dry_run)
            .debug(self.debug || self.verbose >= 2)
            .age_threshold_days(self.age_threshold_days)
            .preserve_binaries(self.preserve_cargo_binaries.to_vec())
            .quiet(self.quiet);

        if let Some(size) = max_size {
            builder = builder.max_target_size(size);
        }

        if let Some(nanos) = last_gc_mtime_nanos {
            builder = builder.previous_build_mtime_nanos(nanos);
        }

        let config = builder.build();

        let stats = config.perform_gc(self.verbose)?;

        if !self.quiet {
            eprintln!("Garbage collection complete:");
            eprintln!("  Initial size: {}", gc::format_size(stats.initial_size));
            eprintln!("  Final size: {}", gc::format_size(stats.final_size));
            eprintln!("  Space freed: {}", gc::format_size(stats.bytes_freed));
            eprintln!("  Artifacts removed: {}", stats.artifacts_removed);
            eprintln!("  Crates cleaned: {}", stats.crates_cleaned);
            eprintln!("  Binaries preserved: {}", stats.binaries_preserved);

            if let Some(cap) = max_size {
                let mode = if auto_cap_used { "auto" } else { "user" };
                eprintln!("  Cap used ({}): {}", mode, gc::format_size(cap));
            }

            if self.dry_run {
                eprintln!("  (DRY RUN - no files were actually deleted)");
            }
        }

        if let Some(path) = self.metadata_path {
            let mut metadata = loaded_metadata.unwrap_or_else(StateMetadata::new);
            metadata.gc_metrics.runs = metadata.gc_metrics.runs.saturating_add(1);
            if let Some(size) = current_size {
                metadata.gc_metrics.seed_initial_size.get_or_insert(size);
            }
            push_bounded(
                &mut metadata.gc_metrics.recent_initial_sizes,
                stats.initial_size,
            );
            push_bounded(
                &mut metadata.gc_metrics.recent_bytes_freed,
                stats.bytes_freed,
            );
            push_bounded(
                &mut metadata.gc_metrics.recent_final_sizes,
                stats.final_size,
            );
            if auto_cap_used {
                metadata.gc_metrics.last_suggested_cap = max_size;
                metadata.gc_metrics.last_cap_trace = cap_trace.clone();
            }

            save_metadata(&metadata, path)?;
        }

        Ok(())
    }
}

pub(crate) fn push_bounded(vec: &mut Vec<u64>, value: u64) {
    vec.push(value);
    if vec.len() > GC_METRICS_WINDOW {
        let overflow = vec.len() - GC_METRICS_WINDOW;
        vec.drain(0..overflow);
    }
}

pub(crate) fn suggest_max_target_size(
    metrics: &GcMetrics,
    seed_from_current: Option<u64>,
) -> Option<(u64, CapTrace)> {
    let seed = metrics.seed_initial_size.or(seed_from_current)?;

    let finals = finals_from_metrics(metrics, seed);
    let growths = growths_from_metrics(metrics, &finals, seed);
    let final_growths = positive_final_growths(&finals);
    let baseline = baseline_from_finals(&finals);
    let has_prev_cap = metrics.last_suggested_cap.is_some();
    let growth_budget = growth_budget_from_growths(&growths, has_prev_cap);

    let mut proposed = baseline.saturating_add(growth_budget);

    let mut clamp_reason = "none".to_string();

    if let Some(prev_cap) = metrics.last_suggested_cap {
        // If observed growth (based on finals) is within a deadband, hold the cap
        // steady.
        let observed_p90 = percentile(&final_growths, 90);
        let growth_pct = if baseline == 0 {
            0
        } else {
            observed_p90.saturating_mul(100) / baseline
        };

        if observed_p90 == 0 {
            // No observed positive growth; hold steady when baseline is at/above the cap,
            // otherwise allow the shrink clamp to apply.
            if baseline >= prev_cap {
                proposed = prev_cap;
                clamp_reason = "deadband/hold".to_string();
            }
        } else if growth_pct <= GROWTH_DEADBAND_PCT {
            proposed = prev_cap;
            clamp_reason = "deadband/hold".to_string();
        }

        let max_up = prev_cap + prev_cap.saturating_mul(MAX_GROWTH_FACTOR_PER_RUN_PCT) / 100;
        let max_down =
            prev_cap.saturating_sub(prev_cap.saturating_mul(MAX_SHRINK_FACTOR_PER_RUN_PCT) / 100);

        let baseline_lower = baseline.min(max_up).min(prev_cap);
        let lower = max_down.max(baseline_lower).min(max_up);

        let clamped = proposed.clamp(lower, max_up);
        if clamped != proposed && clamp_reason == "none" {
            clamp_reason = if clamped == max_up {
                "clamped:+growth"
            } else if clamped == max_down {
                "clamped:-shrink"
            } else {
                "clamped:baseline"
            }
            .to_string();
        } else if clamp_reason == "none" {
            clamp_reason = "within-window".to_string();
        }
        proposed = clamped;
    } else {
        proposed = proposed.max(baseline);
        clamp_reason = "cold-start".to_string();
    }

    let max_final = finals.iter().copied().max().unwrap_or(baseline);
    let hard_ceiling = max_final.saturating_mul(2);
    if proposed > hard_ceiling {
        proposed = hard_ceiling;
        clamp_reason = "hard-ceiling".to_string();
    }

    let observed_growth_pct = if baseline == 0 {
        0
    } else {
        percentile(&final_growths, 90).saturating_mul(100) / baseline.max(1)
    };

    Some((
        proposed,
        CapTrace {
            baseline,
            growth_budget,
            observed_growth_pct,
            clamp_reason,
        },
    ))
}

pub(crate) fn percentile(sorted: &[u64], p: u32) -> u64 {
    if sorted.is_empty() {
        return 0;
    }

    let idx = (((sorted.len() - 1) as u128 * p as u128 + 50) / 100) as usize;
    sorted.get(idx).copied().unwrap_or(*sorted.last().unwrap())
}

fn finals_from_metrics(metrics: &GcMetrics, seed: u64) -> Vec<u64> {
    let len = metrics
        .recent_initial_sizes
        .len()
        .min(metrics.recent_bytes_freed.len());

    let mut finals: Vec<u64> = (0..len)
        .map(|i| metrics.recent_initial_sizes[i].saturating_sub(metrics.recent_bytes_freed[i]))
        .collect();

    if finals.is_empty() {
        finals.push(seed);
    }

    finals
}

fn growths_from_metrics(metrics: &GcMetrics, finals: &[u64], seed: u64) -> Vec<u64> {
    let len = finals.len().min(metrics.recent_initial_sizes.len());

    let mut growths: Vec<u64> = Vec::with_capacity(len.saturating_sub(1));
    for i in 1..len {
        let prev_final = finals.get(i - 1).copied().unwrap_or(seed);
        let init = metrics.recent_initial_sizes[i];
        growths.push(init.saturating_sub(prev_final));
    }

    growths
}

fn baseline_from_finals(finals: &[u64]) -> u64 {
    let mut finals_sorted = finals.to_vec();
    finals_sorted.sort_unstable();
    percentile(&finals_sorted, 50)
}

fn growth_budget_from_growths(growths: &[u64], has_prev_cap: bool) -> u64 {
    // Only consider positive growth when sizing headroom.
    let mut positives: Vec<u64> = growths.iter().copied().filter(|g| *g > 0).collect();

    if positives.is_empty() {
        // Steady-state: keep a small cushion instead of re-adding the full cold-start
        // headroom.
        return if has_prev_cap {
            MIN_STEADY_HEADROOM_BYTES
        } else {
            MIN_HEADROOM_BYTES
        };
    }

    positives.sort_unstable();
    let p90 = percentile(&positives, 90);

    if has_prev_cap {
        p90.max(MIN_STEADY_HEADROOM_BYTES)
    } else {
        p90.max(MIN_HEADROOM_BYTES)
    }
}

fn positive_final_growths(finals: &[u64]) -> Vec<u64> {
    let mut growths: Vec<u64> = finals
        .windows(2)
        .filter_map(|w| w.get(1).zip(w.first()).map(|(b, a)| b.saturating_sub(*a)))
        .filter(|g| *g > 0)
        .collect();

    growths.sort_unstable();
    growths
}
