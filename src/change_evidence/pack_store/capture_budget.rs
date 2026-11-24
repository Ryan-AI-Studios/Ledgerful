use super::MemoryBudget;
use std::{
    cell::Cell,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

const CAPTURE_DURATION: Duration = Duration::from_secs(30);
const CAPTURE_BYTES: usize = 256 * 1024 * 1024;

pub(crate) struct CaptureBudget {
    deadline: Cell<Instant>,
    paused: Cell<bool>,
    aggregate: AtomicUsize,
    memory: MemoryBudget,
    interrupted: AtomicBool,
}

#[derive(Clone, Copy)]
pub(crate) struct CapturePhase<'a> {
    pub(super) deadline: Instant,
    pub(super) aggregate: &'a AtomicUsize,
    pub(super) memory: &'a MemoryBudget,
    pub(super) interrupted: &'a AtomicBool,
}

impl CapturePhase<'_> {
    #[allow(dead_code)]
    pub(crate) fn remaining_read_bytes(self) -> usize {
        self.aggregate.load(Ordering::Relaxed)
    }
}

impl CaptureBudget {
    pub(crate) fn new() -> Self {
        Self::at(Instant::now(), CAPTURE_DURATION, CAPTURE_BYTES)
    }

    fn at(now: Instant, duration: Duration, bytes: usize) -> Self {
        Self {
            deadline: Cell::new(now.checked_add(duration).unwrap_or(now)),
            paused: Cell::new(false),
            aggregate: AtomicUsize::new(bytes),
            memory: MemoryBudget::new(bytes),
            interrupted: AtomicBool::new(false),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn phase(&self) -> Option<CapturePhase<'_>> {
        self.phase_at(Instant::now())
    }

    fn phase_at(&self, now: Instant) -> Option<CapturePhase<'_>> {
        let deadline = self.deadline.get();
        if self.paused.get() || now >= deadline || self.interrupted.load(Ordering::Acquire) {
            return None;
        }
        Some(CapturePhase {
            deadline,
            aggregate: &self.aggregate,
            memory: &self.memory,
            interrupted: &self.interrupted,
        })
    }

    pub(crate) fn is_invalidated(&self) -> bool {
        self.interrupted.load(Ordering::Acquire)
    }

    pub(crate) fn invalidate(&self) {
        self.interrupted.store(true, Ordering::Release);
    }

    pub(crate) fn pause_command(&self) -> Option<CommandPause<'_>> {
        self.pause_at(Instant::now())
    }

    fn pause_at(&self, started: Instant) -> Option<CommandPause<'_>> {
        self.phase_at(started)?;
        self.paused.set(true);
        Some(CommandPause {
            budget: self,
            started,
        })
    }
}

pub(crate) struct CommandPause<'a> {
    budget: &'a CaptureBudget,
    started: Instant,
}

impl CommandPause<'_> {
    fn resume_at(&self, now: Instant) {
        if !self.budget.paused.replace(false) {
            return;
        }
        let deadline = self.budget.deadline.get();
        let elapsed = now.saturating_duration_since(self.started);
        if let Some(extended) = deadline.checked_add(elapsed) {
            self.budget.deadline.set(extended);
        }
    }
}

impl Drop for CommandPause<'_> {
    fn drop(&mut self) {
        self.resume_at(Instant::now());
    }
}
