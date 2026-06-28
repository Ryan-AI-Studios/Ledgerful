use camino::Utf8PathBuf;
use notify_debouncer_full::{Debouncer, RecommendedCache, new_debouncer, notify::*};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info};

use crate::watch::WatchError;
use crate::watch::batch::{WatchBatch, WatchEvent, WatchEventKind};
use crate::watch::filters::EventFilter;
use crate::watch::normalize::normalize_event_path;

pub type WatchCallback = Box<dyn Fn(WatchBatch) + Send + Sync>;

pub struct Watcher {
    #[allow(dead_code)]
    debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
}

impl Watcher {
    pub fn new(
        paths: Vec<Utf8PathBuf>,
        interval: Duration,
        ignore_patterns: Vec<String>,
        callback: WatchCallback,
    ) -> std::result::Result<Self, WatchError> {
        let filter = EventFilter::new(&ignore_patterns)?;
        let roots: Vec<_> = paths
            .iter()
            .map(|path| path.as_std_path().to_path_buf())
            .collect();
        let callback = Arc::new(Mutex::new(callback));

        let mut debouncer = new_debouncer(
            interval,
            None,
            move |res: notify_debouncer_full::DebounceEventResult| match res {
                Ok(events) => {
                    let mut watch_events = Vec::new();
                    for event in events {
                        let kind = match event.kind {
                            EventKind::Create(_) => WatchEventKind::Create,
                            EventKind::Modify(_) => WatchEventKind::Modify,
                            EventKind::Remove(_) => WatchEventKind::Delete,
                            EventKind::Any => WatchEventKind::Unknown,
                            _ => WatchEventKind::Unknown,
                        };

                        for path in &event.paths {
                            let normalized = roots
                                .iter()
                                .find_map(|root| normalize_event_path(path, root))
                                .or_else(|| Utf8PathBuf::from_path_buf(path.clone()).ok());

                            if let Some(utf8_path) = normalized
                                && filter.is_allowed(&utf8_path)
                            {
                                debug!("Watch event: {:?} on {}", kind, utf8_path);
                                watch_events.push(WatchEvent {
                                    path: utf8_path,
                                    kind: kind.clone(),
                                });
                            }
                        }
                    }

                    if !watch_events.is_empty() {
                        let batch = WatchBatch::new(watch_events);
                        (callback.lock())(batch);
                    }
                }
                Err(errors) => {
                    for err in errors {
                        error!("Watcher error: {}", err);
                    }
                }
            },
        )
        .map_err(|e| WatchError::NotifyError(e.to_string()))?;

        for path in paths {
            debouncer
                .watch(path.as_std_path(), RecursiveMode::Recursive)
                .map_err(|e| WatchError::NotifyError(e.to_string()))?;
            info!("Watching {}", path);
        }

        Ok(Self { debouncer })
    }

    pub fn stop(self) {
        // Debouncer stops on drop
        debug!("Stopping watcher");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::mpsc::channel;
    use tempfile::tempdir;

    #[test]
    fn test_watcher_batching() {
        let tmp = tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();

        let (tx, rx) = channel();
        let callback = Box::new(move |batch: WatchBatch| {
            tx.send(batch).unwrap();
        });

        let _watcher = Watcher::new(
            vec![root.clone()],
            Duration::from_millis(100),
            Vec::new(),
            callback,
        )
        .unwrap();

        // Trigger some events
        let file_path = root.join("test.txt");
        fs::write(&file_path, "hello").unwrap();
        fs::write(&file_path, "world").unwrap();

        // Wait for batch
        let batch = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("Did not receive batch");

        assert!(!batch.events.is_empty());
        assert!(
            batch
                .events
                .iter()
                .any(|e| e.path.as_str().ends_with("test.txt"))
        );
    }
}
