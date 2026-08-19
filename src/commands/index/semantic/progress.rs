use indicatif::ProgressStyle;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Product progress lines (0148 / 0161): never via filterable tracing INFO.
/// Suppressed under `--json` so machine stdout stays pure (B6).
pub(crate) fn emit_semantic_progress(json: bool, message: &str) {
    if !json {
        println!("{message}");
    }
}

/// Non-TTY mid-phase report stride: ~total/20 ticks (no upper clamp).
/// Poller only starts when total > 1 (caller gate).
pub(crate) fn non_tty_progress_step(total: usize) -> usize {
    (total / 20).max(1)
}

/// Soft E (0167 D8): hide ProgressBar/spinner when machine JSON or non-interactive.
/// Pure so interactive-TTY+`--json` is unit-testable without a real TTY.
pub(crate) fn hide_semantic_progress_bars(json: bool, interactive: bool) -> bool {
    json || !interactive
}

/// Soft C (0167 D6): "embedding done" progress line only after successful embed collect.
/// Returns `None` on failure so callers cannot print a false done line.
pub(crate) fn embedding_done_progress_line(chunks: usize, succeeded: bool) -> Option<String> {
    if succeeded {
        Some(format!("Semantic index: embedding done {chunks} chunks…"))
    } else {
        None
    }
}

pub(crate) fn semantic_bar_style(template: &str) -> ProgressStyle {
    ProgressStyle::with_template(template)
        .or_else(|_| ProgressStyle::with_template("{pos}/{len}"))
        .unwrap_or_else(|_| ProgressStyle::default_bar())
}

/// Non-TTY mid-phase counters: AtomicUsize + background poller (no println in Rayon).
pub(crate) struct NonTtyPhaseProgress {
    pub(crate) counter: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl NonTtyPhaseProgress {
    pub(crate) fn start(label: &'static str, total: usize, unit: &'static str, json: bool) -> Self {
        let counter = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = if !json && !crate::util::term::is_interactive() && total > 1 {
            let c = Arc::clone(&counter);
            let s = Arc::clone(&stop);
            // Throttle: every ~total/20 files (no upper clamp), or ~20s wall.
            let step = non_tty_progress_step(total);
            Some(std::thread::spawn(move || {
                let interval = Duration::from_secs(20);
                let mut last_n = 0usize;
                let mut last_t = Instant::now();
                while !s.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(500));
                    let n = c.load(Ordering::Relaxed);
                    if n == 0 {
                        continue;
                    }
                    if n >= total {
                        break;
                    }
                    if n > last_n
                        && (n.saturating_sub(last_n) >= step || last_t.elapsed() >= interval)
                    {
                        println!("Semantic index: {label} {n}/{total} {unit}…");
                        last_n = n;
                        last_t = Instant::now();
                    }
                }
            }))
        } else {
            None
        };
        Self {
            counter,
            stop,
            handle,
        }
    }

    pub(crate) fn finish(self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle {
            let _ = handle.join();
        }
    }
}
