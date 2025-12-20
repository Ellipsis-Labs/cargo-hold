//! Voyage command (anchor + heave).

use std::path::Path;

use crate::commands::anchor::anchor;
use crate::commands::gc_options::{GcOptions, GcOptionsBuilder};
use crate::commands::heave::Heave;
use crate::error::{HoldError, Result};
use crate::logging::Logger;

pub struct Voyage<'a> {
    pub(crate) gc: GcOptions<'a>,
    pub(crate) working_dir: &'a Path,
}

pub struct VoyageBuilder<'a> {
    gc: GcOptionsBuilder<'a>,
    working_dir: Option<&'a Path>,
}

impl<'a> Voyage<'a> {
    pub fn builder() -> VoyageBuilder<'a> {
        VoyageBuilder::new()
    }

    /// Execute the voyage (anchor + heave)
    pub fn run(self) -> Result<()> {
        let log = Logger::new(self.gc.verbose(), self.gc.quiet());
        log.info("🚢 Setting sail on voyage (anchor + heave)...");

        anchor(
            self.gc
                .metadata_path()
                .ok_or_else(|| HoldError::ConfigError("metadata_path is required".to_string()))?,
            self.gc.verbose(),
            self.gc.quiet(),
            self.working_dir,
        )?;

        log.info("🧹 Starting garbage collection...");

        Heave::builder()
            .target_dir(self.gc.target_dir())
            .max_target_size(self.gc.max_target_size())
            .auto_max_target_size(self.gc.auto_max_target_size())
            .dry_run(self.gc.dry_run())
            .debug(self.gc.debug())
            .preserve_cargo_binaries(self.gc.preserve_cargo_binaries())
            .age_threshold_days(self.gc.age_threshold_days())
            .verbose(self.gc.verbose())
            .metadata_path(
                self.gc.metadata_path().ok_or_else(|| {
                    HoldError::ConfigError("metadata_path is required".to_string())
                })?,
            )
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
            working_dir: self
                .working_dir
                .ok_or_else(|| HoldError::ConfigError("working_dir is required".to_string()))?,
        })
    }
}
