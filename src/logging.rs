use std::fmt::Display;

#[derive(Clone, Copy, Debug)]
pub struct Logger {
    verbose: u8,
    quiet: bool,
}

impl Logger {
    pub fn new(verbose: u8, quiet: bool) -> Self {
        Self { verbose, quiet }
    }

    pub fn info(&self, message: impl Display) {
        if !self.quiet {
            eprintln!("{message}");
        }
    }

    pub fn verbose(&self, level: u8, message: impl Display) {
        if !self.quiet && self.verbose >= level {
            eprintln!("{message}");
        }
    }

    pub fn quiet(&self) -> bool {
        self.quiet
    }

    pub fn level(&self) -> u8 {
        self.verbose
    }
}
