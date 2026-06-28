use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

pub struct Spinner {
    pb: ProgressBar,
}

impl Spinner {
    pub fn new(message: impl Into<String>) -> Self {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        pb.set_message(message.into());
        pb.enable_steady_tick(Duration::from_millis(100));
        Self { pb }
    }

    pub fn finish(self) {
        self.pb.finish_and_clear();
    }

    pub fn set_message(&self, message: impl Into<String>) {
        self.pb.set_message(message.into());
    }
}
