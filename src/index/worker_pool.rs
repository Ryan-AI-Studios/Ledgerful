use crate::index::types::{ProjectFile, ProjectSymbol};
use camino::Utf8PathBuf;
use crossbeam::channel::{Receiver, unbounded};
use indicatif::ProgressBar;
use miette::Result;
use std::sync::Arc;

pub enum JobResult {
    Parsed(ProjectFile, Vec<ProjectSymbol>),
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
                rayon::current_num_threads()
            } else {
                num_threads
            },
        }
    }

    pub fn process_parsing<F>(
        &self,
        files: Vec<Utf8PathBuf>,
        pb: Option<ProgressBar>,
        parser: F,
    ) -> Result<Receiver<JobResult>>
    where
        F: Fn(&camino::Utf8Path) -> Result<(ProjectFile, Vec<ProjectSymbol>)>
            + Send
            + Sync
            + 'static,
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
                        Ok((pf, ps)) => {
                            let _ = tx.send(JobResult::Parsed(pf, ps));
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
