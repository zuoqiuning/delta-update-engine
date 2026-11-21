/// Human-readable output that disappears under `--json`, so stdout stays one
/// parseable JSON object.
pub struct Console {
    quiet: bool,
}

impl Console {
    pub fn new(json: bool) -> Self {
        Self { quiet: json }
    }

    pub fn say(&self, text: impl AsRef<str>) {
        if !self.quiet {
            println!("{}", text.as_ref());
        }
    }

    /// Print without adding a newline (used for the generated guide).
    pub fn raw(&self, text: &str) {
        if !self.quiet {
            print!("{text}");
        }
    }

    /// Warnings stay visible under `--json`, but move to stderr so they cannot
    /// corrupt the JSON document on stdout.
    pub fn warn(&self, text: impl AsRef<str>) {
        if self.quiet {
            eprintln!("{}", text.as_ref());
        } else {
            println!("{}", text.as_ref());
        }
    }
}

pub fn info(message: &str) {
    println!("{message}");
}

pub fn warn(message: &str) {
    eprintln!("warning: {message}");
}

pub fn error(message: &str) {
    eprintln!("error: {message}");
}
