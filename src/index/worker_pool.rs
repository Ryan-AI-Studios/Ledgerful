use crate::index::bindings::FileBinding;
use crate::index::types::{ProjectFile, ProjectSymbol};
use camino::Utf8PathBuf;
use crossbeam::channel::{Receiver, unbounded};
use indicatif::ProgressBar;
use miette::Result;
use std::sync::Arc;

/// Payload for a successfully parsed source file (boxed in [`JobResult`] for size).
pub struct ParsedFileJob {
    pub file: ProjectFile,
    pub symbols: Vec<ProjectSymbol>,
    pub bindings: Vec<FileBinding>,
    /// Encoding-aware source captured during `analyze_file` (DoD-1b).
    pub content: Option<Arc<str>>,
}

pub enum JobResult {
    Parsed(Box<ParsedFileJob>),
    Indexed(i64), // file_id
    Enriched,
    Failure(Utf8PathBuf, String),
}

pub struct WorkerPool {
    num_threads: usize,
}

impl WorkerPool {
    pub fn new(num_threads: usize) -> Self {
        Self {
            num_threads: if num_threads == 0 {
                rayon::current_num_threads().clamp(1, 4)
            } else {
                num_threads
            },
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn resolved_threads(&self) -> usize {
        self.num_threads
    }

    pub fn process_parsing<F>(
        &self,
        files: Vec<Utf8PathBuf>,
        pb: Option<ProgressBar>,
        parser: F,
    ) -> Result<Receiver<JobResult>>
    where
        F: Fn(&camino::Utf8Path) -> Result<ParsedFileJob> + Send + Sync + 'static,
    {
        let (tx, rx) = unbounded();
        let parser = Arc::new(parser);
        let pb = pb.map(Arc::new);

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.num_threads)
            .build()
            .map_err(|e| miette::miette!("Failed to build thread pool: {}", e))?;

        std::thread::spawn(move || {
            pool.install(|| {
                use rayon::prelude::*;
                files.into_par_iter().for_each(|path| {
                    match parser(&path) {
                        Ok(job) => {
                            let _ = tx.send(JobResult::Parsed(Box::new(job)));
                        }
                        Err(e) => {
                            let _ = tx.send(JobResult::Failure(path, e.to_string()));
                        }
                    }
                    if let Some(pb) = &pb {
                        pb.inc(1);
                    }
                });
            });
        });

        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    mod env_guard {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/integration/common/env_guard.rs"
        ));
    }
    use super::*;
    use env_guard::TempEnv;
    use serial_test::serial;

    #[test]
    #[serial(env)]
    fn resolved_threads_auto_cap_at_most_four() {
        let auto = WorkerPool::new(0).resolved_threads();
        assert!(
            (1..=4).contains(&auto),
            "new(0) must resolve to 1..=4, got {auto}"
        );

        let _guard = TempEnv::set("RAYON_NUM_THREADS", "32");
        let high = WorkerPool::new(0).resolved_threads();
        assert!(
            (1..=4).contains(&high),
            "new(0) with RAYON_NUM_THREADS=32 must still cap at 1..=4, got {high}"
        );

        assert_eq!(
            WorkerPool::new(8).resolved_threads(),
            8,
            "non-zero new(n) honors n"
        );
    }
}
