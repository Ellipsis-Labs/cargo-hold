//! Heave (garbage collection) command and helpers.

use std::path::Path;

use crate::commands::gc_options::{GcOptions, GcOptionsBuilder};
use crate::error::Result;
use crate::gc::config::Gc;
use crate::gc::{self, auto_cap};
use crate::logging::Logger;
use crate::metadata::{load_metadata, save_metadata};
use crate::state::{CapTrace, StateMetadata};

pub struct Heave<'a> {
    gc: GcOptions<'a>,
}

pub struct HeaveBuilder<'a> {
    gc: GcOptionsBuilder<'a>,
}

impl<'a> Default for HeaveBuilder<'a> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> HeaveBuilder<'a> {
    pub fn new() -> Self {
        Self {
            gc: GcOptionsBuilder::new(),
        }
    }

    pub fn target_dir(mut self, path: &'a Path) -> Self {
        self.gc = self.gc.target_dir(path);
        self
    }

    pub fn max_target_size(mut self, size: Option<&'a str>) -> Self {
        self.gc = self.gc.max_target_size(size);
        self
    }

    pub fn auto_max_target_size(mut self, enabled: bool) -> Self {
        self.gc = self.gc.auto_max_target_size(enabled);
        self
    }

    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.gc = self.gc.dry_run(dry_run);
        self
    }

    pub fn debug(mut self, debug: bool) -> Self {
        self.gc = self.gc.debug(debug);
        self
    }

    pub fn preserve_cargo_binaries(mut self, binaries: &'a [String]) -> Self {
        self.gc = self.gc.preserve_cargo_binaries(binaries);
        self
    }

    pub fn age_threshold_days(mut self, days: u32) -> Self {
        self.gc = self.gc.age_threshold_days(days);
        self
    }

    pub fn verbose(mut self, verbose: u8) -> Self {
        self.gc = self.gc.verbose(verbose);
        self
    }

    pub fn metadata_path(mut self, path: &'a Path) -> Self {
        self.gc = self.gc.metadata_path(path);
        self
    }

    pub fn quiet(mut self, quiet: bool) -> Self {
        self.gc = self.gc.quiet(quiet);
        self
    }

    pub fn build(self) -> Result<Heave<'a>> {
        Ok(Heave {
            gc: self.gc.build()?,
        })
    }
}

impl<'a> Heave<'a> {
    pub fn builder<'b>() -> HeaveBuilder<'b> {
        HeaveBuilder::new()
    }

    /// Execute the heave command (garbage collection)
    pub fn heave(self) -> Result<()> {
        let log = Logger::new(self.gc.verbose(), self.gc.quiet());
        log.verbose(1, "Heave ho! Starting garbage collection...");

        let mut max_size = if let Some(size_str) = self.gc.max_target_size() {
            Some(gc::parse_size(size_str)?)
        } else {
            None
        };

        let loaded_metadata = if let Some(path) = self.gc.metadata_path() {
            match load_metadata(path) {
                Ok(metadata) => Some(metadata),
                Err(err) => {
                    log.info(format!(
                        "Warning: failed to load metadata for GC metrics ({}). Continuing with \
                         defaults.",
                        err
                    ));
                    None
                }
            }
        } else {
            None
        };

        let current_size = gc::calculate_directory_size(self.gc.target_dir())
            .ok()
            .filter(|size| *size > 0);

        let last_gc_mtime_nanos = loaded_metadata.as_ref().and_then(|m| m.last_gc_mtime_nanos);

        if !log.quiet()
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
            && self.gc.auto_max_target_size()
            && let Some(metadata) = loaded_metadata.as_ref()
            && let Some((suggested, trace)) =
                auto_cap::suggest_max_target_size(&metadata.gc_metrics, current_size)
        {
            max_size = Some(suggested);
            auto_cap_used = true;
            cap_trace = Some(trace.clone());
            if !log.quiet()
                && let Some(trace) = cap_trace.as_ref()
            {
                // Always log a concise summary (even without verbose) so CI logs show why the
                // cap moved.
                eprintln!(
                    "Auto-selected max target size: {} (baseline {}, headroom {}, growth p90 {}%, \
                     clamp {})",
                    gc::format_size(suggested),
                    gc::format_size(trace.baseline),
                    gc::format_size(trace.growth_budget),
                    trace.observed_growth_pct,
                    trace.clamp_reason
                );
            }
        }

        let mut builder = Gc::builder()
            .target_dir(self.gc.target_dir().to_path_buf())
            .dry_run(self.gc.dry_run())
            .debug(self.gc.debug() || self.gc.verbose() >= 2)
            .age_threshold_days(self.gc.age_threshold_days())
            .preserve_binaries(self.gc.preserve_cargo_binaries().to_vec())
            .quiet(self.gc.quiet());

        if let Some(size) = max_size {
            builder = builder.max_target_size(size);
        }

        if let Some(nanos) = last_gc_mtime_nanos {
            builder = builder.previous_build_mtime_nanos(nanos);
        }

        let config = builder.build();

        let stats = config.perform_gc(self.gc.verbose())?;

        if !log.quiet() {
            eprintln!("Garbage collection complete:");
            eprintln!("  Initial size: {}", gc::format_size(stats.initial_size));
            eprintln!("  Final size: {}", gc::format_size(stats.final_size));
            eprintln!("  Space freed: {}", gc::format_size(stats.bytes_freed));
            eprintln!("  Artifacts removed: {}", stats.artifacts_removed);
            eprintln!("  Crates cleaned: {}", stats.crates_cleaned);
            eprintln!("  Binaries preserved: {}", stats.binaries_preserved);
            eprintln!(
                "  Registry cleanup: {} files, {} dirs, {} freed",
                stats.registry_files_removed,
                stats.registry_dirs_removed,
                gc::format_size(stats.registry_bytes_freed)
            );

            if let Some(cap) = max_size {
                let mode = if auto_cap_used { "auto" } else { "user" };
                eprintln!("  Cap used ({}): {}", mode, gc::format_size(cap));
            }

            if self.gc.dry_run() {
                eprintln!("  (DRY RUN - no files were actually deleted)");
            }
        }

        if let Some(path) = self.gc.metadata_path() {
            let mut metadata = loaded_metadata.unwrap_or_else(StateMetadata::new);
            metadata.gc_metrics.runs = metadata.gc_metrics.runs.saturating_add(1);
            if let Some(size) = current_size {
                metadata.gc_metrics.seed_initial_size.get_or_insert(size);
            }
            auto_cap::push_bounded(
                &mut metadata.gc_metrics.recent_initial_sizes,
                stats.initial_size,
            );
            auto_cap::push_bounded(
                &mut metadata.gc_metrics.recent_bytes_freed,
                stats.bytes_freed,
            );
            auto_cap::push_bounded(
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
