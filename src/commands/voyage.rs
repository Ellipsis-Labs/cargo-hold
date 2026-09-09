//! Voyage command (anchor + heave).

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::commands::anchor::anchor_with_report;
use crate::commands::gc_options::{GcOptions, GcOptionsBuilder};
use crate::commands::heave::Heave;
use crate::error::{HoldError, Result};
use crate::logging::Logger;
use crate::metadata::load_metadata;

pub struct Voyage<'a> {
    pub(crate) gc: GcOptions<'a>,
    pub(crate) working_dir: &'a Path,
    gc_min_interval_hours: Option<u64>,
    force_gc: bool,
}

pub struct VoyageBuilder<'a> {
    gc: GcOptionsBuilder<'a>,
    working_dir: Option<&'a Path>,
    gc_min_interval_hours: Option<u64>,
    force_gc: bool,
}

impl<'a> Voyage<'a> {
    pub fn builder() -> VoyageBuilder<'a> {
        VoyageBuilder::new()
    }

    /// Execute the voyage (anchor + heave)
    pub fn run(self) -> Result<()> {
        let log = Logger::new(self.gc.verbose(), self.gc.quiet());
        log.info("🚢 Setting sail on voyage (anchor + heave)...");

        let metadata_path = self
            .gc
            .metadata_path()
            .ok_or_else(|| HoldError::ConfigError("metadata_path is required".to_string()))?;

        let anchor_report = anchor_with_report(
            metadata_path,
            self.gc.verbose(),
            self.gc.quiet(),
            self.working_dir,
        )?;
        let source_changed = anchor_report.has_source_changes();

        if source_changed {
            log.verbose(
                1,
                format!(
                    "Source changes detected ({} modified, {} added); skipping artifact mtime \
                     refresh before build",
                    anchor_report.modified_files, anchor_report.added_files
                ),
            );
        } else {
            log.verbose(
                2,
                format!(
                    "No source changes detected across {} unchanged files",
                    anchor_report.unchanged_files
                ),
            );
        }

        let reason = if self.force_gc {
            "forced by --force-gc"
        } else if let Some(hours) = self.gc_min_interval_hours {
            if let Some(last_gc) = load_metadata(metadata_path)?.last_gc_mtime_nanos {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();
                let interval = u128::from(hours) * 3_600 * 1_000_000_000;
                if now.saturating_sub(last_gc) < interval {
                    log.info(format!(
                        "🧹 Garbage collection skipped: within {hours}-hour cooldown"
                    ));
                    log.info("🚢 Voyage completed successfully!");
                    return Ok(());
                }
                "cooldown expired"
            } else {
                "no previous GC timestamp"
            }
        } else {
            "no GC interval configured"
        };
        log.info(format!("🧹 Starting garbage collection: {reason}"));

        Heave::builder()
            .target_dir(self.gc.target_dir())
            .max_target_size(self.gc.max_target_size())
            .auto_max_target_size(self.gc.auto_max_target_size())
            .dry_run(self.gc.dry_run())
            .debug(self.gc.debug())
            .preserve_cargo_binaries(self.gc.preserve_cargo_binaries())
            .age_threshold_days(self.gc.age_threshold_days())
            .verbose(self.gc.verbose())
            .metadata_path(metadata_path)
            .rejuvenate_artifact_mtimes(!source_changed)
            .quiet(self.gc.quiet())
            .build()?
            .heave()?;

        log.info("🚢 Voyage completed successfully!");

        Ok(())
    }
}

impl<'a> Default for VoyageBuilder<'a> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> VoyageBuilder<'a> {
    pub fn new() -> Self {
        Self {
            gc: GcOptionsBuilder::new(),
            working_dir: None,
            gc_min_interval_hours: None,
            force_gc: false,
        }
    }

    pub fn metadata_path(mut self, path: &'a Path) -> Self {
        self.gc = self.gc.metadata_path(path);
        self
    }

    pub fn target_dir(mut self, path: &'a Path) -> Self {
        self.gc = self.gc.target_dir(path);
        self
    }

    pub fn max_target_size(mut self, size: Option<&'a str>) -> Self {
        self.gc = self.gc.max_target_size(size);
        self
    }

    pub fn gc_min_interval_hours(mut self, hours: Option<u64>) -> Self {
        self.gc_min_interval_hours = hours;
        self
    }

    pub fn force_gc(mut self, force: bool) -> Self {
        self.force_gc = force;
        self
    }

    pub fn gc_dry_run(mut self, dry_run: bool) -> Self {
        self.gc = self.gc.dry_run(dry_run);
        self
    }

    pub fn gc_debug(mut self, debug: bool) -> Self {
        self.gc = self.gc.debug(debug);
        self
    }

    pub fn gc_auto_max_target_size(mut self, enabled: bool) -> Self {
        self.gc = self.gc.auto_max_target_size(enabled);
        self
    }

    pub fn preserve_cargo_binaries(mut self, binaries: &'a [String]) -> Self {
        self.gc = self.gc.preserve_cargo_binaries(binaries);
        self
    }

    pub fn gc_age_threshold_days(mut self, days: u32) -> Self {
        self.gc = self.gc.age_threshold_days(days);
        self
    }

    pub fn verbose(mut self, verbose: u8) -> Self {
        self.gc = self.gc.verbose(verbose);
        self
    }

    pub fn quiet(mut self, quiet: bool) -> Self {
        self.gc = self.gc.quiet(quiet);
        self
    }

    pub fn working_dir(mut self, working_dir: &'a Path) -> Self {
        self.working_dir = Some(working_dir);
        self
    }

    pub fn build(self) -> Result<Voyage<'a>> {
        Ok(Voyage {
            gc: self.gc.build()?,
            gc_min_interval_hours: self.gc_min_interval_hours,
            force_gc: self.force_gc,
            working_dir: self
                .working_dir
                .ok_or_else(|| HoldError::ConfigError("working_dir is required".to_string()))?,
        })
    }
}
