use std::path::Path;

use crate::error::{HoldError, Result};

pub struct GcOptions<'a> {
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

impl<'a> GcOptions<'a> {
    pub fn target_dir(&self) -> &'a Path {
        self.target_dir
    }

    pub fn max_target_size(&self) -> Option<&'a str> {
        self.max_target_size
    }

    pub fn auto_max_target_size(&self) -> bool {
        self.auto_max_target_size
    }

    pub fn dry_run(&self) -> bool {
        self.dry_run
    }

    pub fn debug(&self) -> bool {
        self.debug
    }

    pub fn preserve_cargo_binaries(&self) -> &'a [String] {
        self.preserve_cargo_binaries
    }

    pub fn age_threshold_days(&self) -> u32 {
        self.age_threshold_days
    }

    pub fn verbose(&self) -> u8 {
        self.verbose
    }

    pub fn metadata_path(&self) -> Option<&'a Path> {
        self.metadata_path
    }

    pub fn quiet(&self) -> bool {
        self.quiet
    }
}

pub struct GcOptionsBuilder<'a> {
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

impl<'a> Default for GcOptionsBuilder<'a> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> GcOptionsBuilder<'a> {
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

    pub fn build(self) -> Result<GcOptions<'a>> {
        Ok(GcOptions {
            target_dir: self
                .target_dir
                .ok_or_else(|| HoldError::ConfigError("target_dir is required".to_string()))?,
            max_target_size: self.max_target_size,
            auto_max_target_size: self.auto_max_target_size,
            dry_run: self.dry_run,
            debug: self.debug,
            preserve_cargo_binaries: self.preserve_cargo_binaries,
            age_threshold_days: self.age_threshold_days,
            verbose: self.verbose,
            metadata_path: self.metadata_path,
            quiet: self.quiet,
        })
    }
}
