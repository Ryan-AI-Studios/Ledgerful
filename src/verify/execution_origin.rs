use super::VerificationResult;

pub(crate) const MAX_CAPTURE_POSITIONS: usize = 32;

#[derive(Clone, Copy)]
pub(crate) struct RecordedOutcome {
    pub(crate) position: usize,
    pub(crate) exit_code: i32,
    pub(crate) duration_ms: u64,
    pub(crate) timed_out: bool,
}

/// Created only by finishing a collector from the normal return path.
pub(crate) struct ExecutionTerminal {
    planned: usize,
    completed: usize,
    stopped_on_timeout: bool,
    sample: Vec<RecordedOutcome>,
}

impl ExecutionTerminal {
    pub(crate) fn planned(&self) -> usize {
        self.planned
    }

    pub(crate) fn completed(&self) -> usize {
        self.completed
    }

    pub(crate) fn partial(&self) -> bool {
        self.completed < self.planned
    }

    pub(crate) fn outcomes(&self) -> &[RecordedOutcome] {
        &self.sample
    }

    pub(crate) fn is_consistent(&self) -> bool {
        self.completed <= self.planned
            && (!self.partial() || self.stopped_on_timeout)
            && (!self.stopped_on_timeout || self.completed != 0)
            && self.sample.len() == self.completed.min(MAX_CAPTURE_POSITIONS)
            && self.sample.iter().enumerate().all(|(position, entry)| {
                entry.position == position
                    && entry.timed_out
                        == (self.stopped_on_timeout && position + 1 == self.completed)
            })
    }
}

pub(super) struct ExecutionCollector(ExecutionTerminal);

impl ExecutionCollector {
    pub(super) fn new(planned: usize) -> Self {
        Self(ExecutionTerminal {
            planned,
            completed: 0,
            stopped_on_timeout: false,
            sample: Vec::new(),
        })
    }

    pub(super) fn record_finished(&mut self, position: usize, result: &VerificationResult) {
        self.record(position, result, false);
    }

    pub(super) fn record_timeout(&mut self, position: usize, result: &VerificationResult) {
        self.record(position, result, true);
    }

    fn record(&mut self, position: usize, result: &VerificationResult, timed_out: bool) {
        // Check even positions beyond the sample. An impossible capture is
        // suppressed without altering the verification run.
        if position != self.0.completed || position >= self.0.planned || self.0.stopped_on_timeout {
            self.0.completed = usize::MAX;
            return;
        }
        self.0.completed += 1;
        self.0.stopped_on_timeout = timed_out;
        if position < MAX_CAPTURE_POSITIONS {
            self.0.sample.push(RecordedOutcome {
                position,
                exit_code: result.exit_code,
                duration_ms: result.duration_ms,
                timed_out,
            });
        }
    }

    pub(super) fn complete(self) -> ExecutionTerminal {
        self.0
    }
}
