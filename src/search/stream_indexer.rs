use crate::search::encoding::{normalize_to_utf8, strip_control_characters};
use crate::search::tantivy_engine::TantivySearchEngine;
use crate::search::trigram::extract_trigrams;
use camino::{Utf8Path, Utf8PathBuf};
use crossbeam::channel::bounded;
use miette::{IntoDiagnostic, Result};
use std::fs;
use std::thread;
use tantivy::TantivyDocument;
use tracing::debug;

pub struct StreamIndexer {
    engine: std::sync::Arc<TantivySearchEngine>,
}

struct IndexJob {
    path: Utf8PathBuf,
    content: Vec<u8>,
}

impl StreamIndexer {
    pub fn new(engine: TantivySearchEngine) -> Self {
        Self {
            engine: std::sync::Arc::new(engine),
        }
    }

    pub fn index_repository(&self, root: &Utf8Path) -> Result<()> {
        let (job_tx, job_rx) = bounded::<IndexJob>(100);
        let num_workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(4); // Cap workers to avoid resource exhaustion

        debug!("Starting indexing with {} workers", num_workers);
        let writer = std::sync::Arc::new(self.engine.get_writer(100_000_000)?);

        let mut workers = Vec::new();
        for i in 0..num_workers {
            let rx = job_rx.clone();
            let engine = self.engine.clone();
            let writer = writer.clone();
            let worker = thread::spawn(move || {
                let schema = engine.schema();
                let path_field = match schema.get_field("path") {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::error!("Worker {}: missing path field: {}", i, e);
                        return;
                    }
                };
                let content_field = schema.get_field("content").unwrap();
                let line_count_field = schema.get_field("line_count").unwrap();
                let trigrams_field = schema.get_field("trigrams").unwrap();

                for job in rx {
                    debug!("Worker {}: Indexing file: {}", i, job.path);
                    let utf8_content = normalize_to_utf8(&job.content);
                    let clean_content = strip_control_characters(&utf8_content);
                    let line_count = clean_content.lines().count();

                    // Extract trigrams and join with space for indexing
                    let trigrams = extract_trigrams(&clean_content);
                    let trigrams_str = trigrams.into_iter().collect::<Vec<String>>().join(" ");

                    let mut doc = TantivyDocument::default();
                    doc.add_text(path_field, job.path.as_str());
                    doc.add_text(content_field, &clean_content);
                    doc.add_u64(line_count_field, line_count as u64);
                    doc.add_text(trigrams_field, &trigrams_str);

                    if let Err(e) = writer.add_document(doc) {
                        tracing::error!("Worker {}: failed to add document {}: {}", i, job.path, e);
                        break;
                    }
                }
            });
            workers.push(worker);
        }

        drop(job_rx); // Close original rx in main thread

        let walker = ignore::WalkBuilder::new(root)
            .hidden(true) // Skip hidden files/dirs like .git, .ledgerful
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .build();

        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("Error walking directory: {}", e);
                    continue;
                }
            };

            if entry.file_type().is_some_and(|ft| ft.is_file()) {
                let path = entry.path();
                let utf8_path = Utf8PathBuf::from_path_buf(path.to_path_buf())
                    .map_err(|_| miette::miette!("Invalid UTF-8 path: {:?}", path))?;

                // Skip large files (> 1MB)
                let metadata = entry.metadata().into_diagnostic()?;
                if metadata.len() > 1_000_000 {
                    continue;
                }

                if let Ok(content) = fs::read(&utf8_path) {
                    let relative_path = utf8_path
                        .strip_prefix(root)
                        .unwrap_or(&utf8_path)
                        .to_path_buf();
                    job_tx
                        .send(IndexJob {
                            path: relative_path,
                            content,
                        })
                        .into_diagnostic()?;
                }
            }
        }

        drop(job_tx); // Signals workers to finish

        for worker in workers {
            worker.join().unwrap();
        }

        let mut writer = std::sync::Arc::into_inner(writer)
            .ok_or_else(|| miette::miette!("Failed to get unique access to writer for commit"))?;

        writer.commit().into_diagnostic()?;
        writer.wait_merging_threads().into_diagnostic()?;

        let segment_count = self.engine.segment_count()?;
        debug!("Tantivy index committed with {} segments", segment_count);

        Ok(())
    }
}
