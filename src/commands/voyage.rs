//! Voyage command (anchor + heave).

use std::path::Path;

use crate::commands::anchor::anchor;
use crate::commands::heave::Heave;
use crate::error::{HoldError, Result};

pub struct Voyage<'a> {
    pub(crate) metadata_path: &'a Path,
    pub(crate) target_dir: &'a Path,
    pub(crate) max_target_size: Option<&'a str>,
    pub(crate) gc_dry_run: bool,
    pub(crate) gc_debug: bool,
    pub(crate) preserve_cargo_binaries: &'a [String],
    pub(crate) gc_age_threshold_days: u32,
    pub(crate) gc_auto_max_target_size: bool,
    pub(crate) verbose: u8,
    pub(crate) working_dir: &'a Path,
    pub(crate) quiet: bool,
}

pub struct VoyageBuilder<'a> {
    metadata_path: Option<&'a Path>,
    target_dir: Option<&'a Path>,
    max_target_size: Option<&'a str>,
    gc_dry_run: bool,
    gc_debug: bool,
    preserve_cargo_binaries: &'a [String],
    gc_age_threshold_days: u32,
    gc_auto_max_target_size: bool,
    verbose: u8,
    working_dir: Option<&'a Path>,
    quiet: bool,
}

impl Default for VoyageBuilder<'_> {
    fn default() -> Self {
        Self {
            metadata_path: None,
            target_dir: None,
            max_target_size: None,
            gc_dry_run: false,
            gc_debug: false,
            preserve_cargo_binaries: &[],
            gc_age_threshold_days: 7,
            gc_auto_max_target_size: true,
            verbose: 0,
            working_dir: None,
            quiet: false,
        }
    }
}

impl<'a> Voyage<'a> {
    pub fn builder() -> VoyageBuilder<'a> {
        VoyageBuilder::new()
    }

    /// Execute the voyage (anchor + heave)
    pub fn run(self) -> Result<()> {
        if !self.quiet {
            eprintln!("🚢 Setting sail on voyage (anchor + heave)...");
        }

        anchor(
            self.metadata_path,
            self.verbose,
            self.quiet,
            self.working_dir,
        )?;

        if !self.quiet {
            eprintln!("🧹 Starting garbage collection...");
        }

        Heave::builder()
            .target_dir(self.target_dir)
            .max_target_size(self.max_target_size)
            .auto_max_target_size(self.gc_auto_max_target_size)
            .dry_run(self.gc_dry_run)
            .debug(self.gc_debug)
            .preserve_cargo_binaries(self.preserve_cargo_binaries)
            .age_threshold_days(self.gc_age_threshold_days)
            .verbose(self.verbose)
            .metadata_path(self.metadata_path)
            .quiet(self.quiet)
            .build()
            .heave()?;

        if !self.quiet {
            eprintln!("🚢 Voyage completed successfully!");
        }

        Ok(())
    }
}

impl<'a> VoyageBuilder<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn metadata_path(mut self, path: &'a Path) -> Self {
        self.metadata_path = Some(path);
        self
    }

    pub fn target_dir(mut self, path: &'a Path) -> Self {
        self.target_dir = Some(path);
        self
    }

    pub fn max_target_size(mut self, size: Option<&'a str>) -> Self {
        self.max_target_size = size;
        self
    }

    pub fn gc_dry_run(mut self, dry_run: bool) -> Self {
        self.gc_dry_run = dry_run;
        self
    }

    pub fn gc_debug(mut self, debug: bool) -> Self {
        self.gc_debug = debug;
        self
    }

    pub fn gc_auto_max_target_size(mut self, enabled: bool) -> Self {
        self.gc_auto_max_target_size = enabled;
        self
    }

    pub fn preserve_cargo_binaries(mut self, binaries: &'a [String]) -> Self {
        self.preserve_cargo_binaries = binaries;
        self
    }

    pub fn gc_age_threshold_days(mut self, days: u32) -> Self {
        self.gc_age_threshold_days = days;
        self
    }

    pub fn verbose(mut self, verbose: u8) -> Self {
        self.verbose = verbose;
        self
    }

    pub fn quiet(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
        self
    }

    pub fn working_dir(mut self, working_dir: &'a Path) -> Self {
        self.working_dir = Some(working_dir);
        self
    }

    pub fn build(self) -> Result<Voyage<'a>> {
        Ok(Voyage {
            metadata_path: self
                .metadata_path
                .ok_or_else(|| HoldError::ConfigError("metadata_path is required".to_string()))?,
            target_dir: self
                .target_dir
                .ok_or_else(|| HoldError::ConfigError("target_dir is required".to_string()))?,
            max_target_size: self.max_target_size,
            gc_dry_run: self.gc_dry_run,
            gc_debug: self.gc_debug,
            preserve_cargo_binaries: self.preserve_cargo_binaries,
            gc_age_threshold_days: self.gc_age_threshold_days,
            gc_auto_max_target_size: self.gc_auto_max_target_size,
            verbose: self.verbose,
            working_dir: self
                .working_dir
                .ok_or_else(|| HoldError::ConfigError("working_dir is required".to_string()))?,
            quiet: self.quiet,
        })
    }
}
