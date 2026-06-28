//! Auto-tuner for semantic indexing concurrency.
//!
//! Resolves how many rayon threads to use for AST parsing and how many concurrent
//! embedding requests to issue against the local model. Distinguishes between
//! CPU-bound work (parse), which scales with logical cores, and network-bound
//! work (embed), which is capped to avoid overwhelming the local ONNX server
//! (see commit `90da256` — embedding callsites crashed at 16+ concurrent).
//!
//! Precedence: CLI override (`-j`) > `[semantic].concurrency` config > auto.
//! Auto uses [`std::thread::available_parallelism`] so cgroup/affinity limits
//! are respected under WSL, Docker, and CI containers.

use crate::config::model::SemanticConfig;
use std::num::NonZeroUsize;

/// Resolved thread counts for semantic indexing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedConcurrency {
    /// Threads for CPU-bound AST parsing (rayon pool size).
    pub parse_threads: NonZeroUsize,
    /// Source trace for parse threads.
    pub parse_source: ConcurrencySource,
    /// Max concurrent embedding requests in flight.
    pub embed_threads: NonZeroUsize,
    /// Source trace for embed threads.
    pub embed_source: ConcurrencySource,
    /// Max resolved embed cap.
    pub embed_cap: NonZeroUsize,
    /// Source trace for embed cap.
    pub cap_source: ConcurrencySource,
    /// Requested embed threads before clamping to the cap.
    pub requested_embed_threads: NonZeroUsize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcurrencySource {
    Cli,
    ConfigParse,
    ConfigEmbed,
    ConfigEmbedCap,
    ConfigLegacy,
    ConfigLocalModel,
    Default,
    Auto,
}

impl std::fmt::Display for ConcurrencySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cli => write!(f, "CLI (-j)"),
            Self::ConfigParse => write!(f, "[semantic].parse_concurrency"),
            Self::ConfigEmbed => write!(f, "[semantic].embed_concurrency"),
            Self::ConfigEmbedCap => write!(f, "[semantic].embed_concurrency_cap"),
            Self::ConfigLegacy => write!(f, "[semantic].concurrency (legacy)"),
            Self::ConfigLocalModel => write!(f, "[local_model].concurrency"),
            Self::Default => write!(f, "default"),
            Self::Auto => write!(f, "auto-detected"),
        }
    }
}

/// Options controlling the auto-tuner.
#[derive(Debug, Clone, Copy)]
pub struct ResolveOptions {
    /// Upper bound on concurrent embed requests. Defaults to 4.
    pub embed_cap: NonZeroUsize,
    /// Source trace for the cap.
    pub cap_source: ConcurrencySource,
    /// Override for the "auto" branch. `None` reads
    /// [`std::thread::available_parallelism`].
    pub available_parallelism: Option<NonZeroUsize>,
}

impl Default for ResolveOptions {
    fn default() -> Self {
        Self {
            embed_cap: NonZeroUsize::new(DEFAULT_EMBED_CAP)
                .expect("DEFAULT_EMBED_CAP must be non-zero"),
            cap_source: ConcurrencySource::Default,
            available_parallelism: None,
        }
    }
}

/// Default cap on concurrent embed requests. Picked to stay below the local
/// ONNX server's crash threshold (see commit `90da256`).
pub const DEFAULT_EMBED_CAP: usize = 4;

/// Counting semaphore guarding concurrent embed calls. Backed by a
/// `std::sync::atomic::AtomicUsize` core and `parking_lot::Mutex` / `parking_lot::Condvar` for blocking.
///
/// Each call to [`acquire`] blocks until a permit is available, then returns
/// a [`EmbedPermit`] that releases the slot on drop.
#[derive(Debug)]
pub struct EmbedSemaphore {
    available: std::sync::atomic::AtomicUsize,
    lock: parking_lot::Mutex<()>,
    cond: parking_lot::Condvar,
}

impl EmbedSemaphore {
    pub fn new(permits: usize) -> Self {
        let permits = permits.max(1);
        Self {
            available: std::sync::atomic::AtomicUsize::new(permits),
            lock: parking_lot::Mutex::new(()),
            cond: parking_lot::Condvar::new(),
        }
    }

    /// Block until a permit is available, returning a guard.
    pub fn acquire(&self) -> EmbedPermit<'_> {
        // Spin a bit first as an optimization
        if let Some(permit) = self.try_acquire_spin(100) {
            return permit;
        }

        let mut guard = self.lock.lock();
        loop {
            if let Some(permit) = self.try_acquire() {
                return permit;
            }
            self.cond.wait(&mut guard);
        }
    }

    /// Non-blocking permit acquisition. Returns `Some(permit)` on success,
    /// or `None` if the semaphore has no available permits.
    pub fn try_acquire(&self) -> Option<EmbedPermit<'_>> {
        let mut curr = self.available.load(std::sync::atomic::Ordering::Acquire);
        loop {
            if curr == 0 {
                return None;
            }
            match self.available.compare_exchange_weak(
                curr,
                curr - 1,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(EmbedPermit {
                        semaphore: self,
                        is_noop: false,
                    });
                }
                Err(actual) => curr = actual,
            }
        }
    }

    /// Spin-backoff non-blocking acquisition. Spins up to `max_iters` times before giving up.
    pub fn try_acquire_spin(&self, max_iters: usize) -> Option<EmbedPermit<'_>> {
        for _ in 0..max_iters {
            if let Some(permit) = self.try_acquire() {
                return Some(permit);
            }
            std::hint::spin_loop();
        }
        None
    }

    #[cfg(test)]
    fn available(&self) -> usize {
        self.available.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// RAII guard for an embed slot. Releases on drop.
pub struct EmbedPermit<'a> {
    semaphore: &'a EmbedSemaphore,
    is_noop: bool,
}

impl<'a> EmbedPermit<'a> {
    /// Returns a no-op permit that does not count towards the concurrency cap.
    pub fn noop(semaphore: &'a EmbedSemaphore) -> Self {
        Self {
            semaphore,
            is_noop: true,
        }
    }
}

impl Drop for EmbedPermit<'_> {
    fn drop(&mut self) {
        if self.is_noop {
            return;
        }
        self.semaphore
            .available
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        // Wake up a blocked acquire thread
        let _guard = self.semaphore.lock.lock();
        self.semaphore.cond.notify_one();
    }
}

/// Resolve parse/embed thread counts and cap given the split config model,
/// optional CLI overrides, and tuning options.
pub fn resolve_split_semantic_concurrency(
    cli_override: Option<usize>,
    config: &SemanticConfig,
    legacy_local_concurrency: Option<usize>,
    opts: ResolveOptions,
) -> ResolvedConcurrency {
    // 1. Resolve Cap
    let (embed_cap, cap_source) = if let Some(cap) = config.semantic_embed_concurrency_cap() {
        (
            NonZeroUsize::new(cap).unwrap_or(NonZeroUsize::new(1).unwrap()),
            ConcurrencySource::ConfigEmbedCap,
        )
    } else {
        (opts.embed_cap, opts.cap_source)
    };

    // 2. Resolve Parse Threads
    let (parse_threads, parse_source) = if let Some(cli) = cli_override.filter(|n| *n > 0) {
        (NonZeroUsize::new(cli).unwrap(), ConcurrencySource::Cli)
    } else if let Some(parse) = config.semantic_parse_concurrency() {
        (
            NonZeroUsize::new(parse).unwrap(),
            ConcurrencySource::ConfigParse,
        )
    } else if let Some(legacy) = config.semantic_concurrency() {
        tracing::warn!(
            "Use of deprecated configuration field [semantic].concurrency. Please use parse_concurrency and embed_concurrency instead."
        );
        (
            NonZeroUsize::new(legacy).unwrap(),
            ConcurrencySource::ConfigLegacy,
        )
    } else if let Some(local) = legacy_local_concurrency.filter(|n| *n > 0) {
        (
            NonZeroUsize::new(local).unwrap(),
            ConcurrencySource::ConfigLocalModel,
        )
    } else {
        let auto_val = opts.available_parallelism.map_or(1, |n| n.get()).max(1);
        (
            NonZeroUsize::new(auto_val).unwrap(),
            ConcurrencySource::Auto,
        )
    };

    // 3. Resolve Embed Threads
    let (embed_threads, embed_source) = if let Some(cli) = cli_override.filter(|n| *n > 0) {
        (NonZeroUsize::new(cli).unwrap(), ConcurrencySource::Cli)
    } else if let Some(embed) = config.semantic_embed_concurrency() {
        (
            NonZeroUsize::new(embed).unwrap(),
            ConcurrencySource::ConfigEmbed,
        )
    } else if let Some(legacy) = config.semantic_concurrency() {
        (
            NonZeroUsize::new(legacy).unwrap(),
            ConcurrencySource::ConfigLegacy,
        )
    } else {
        let default_val = DEFAULT_EMBED_CAP.max(1);
        (
            NonZeroUsize::new(default_val).unwrap(),
            ConcurrencySource::Default,
        )
    };

    // Clamp effective embed threads to the resolved cap
    let final_embed = std::cmp::min(embed_threads, embed_cap);

    ResolvedConcurrency {
        parse_threads,
        parse_source,
        embed_threads: final_embed,
        embed_source,
        embed_cap,
        cap_source,
        requested_embed_threads: embed_threads,
    }
}

/// Legacy resolver wrapping the split implementation for compatibility.
pub fn resolve_semantic_concurrency(
    cli_override: Option<usize>,
    config_value: Option<usize>,
    opts: ResolveOptions,
) -> ResolvedConcurrency {
    let config = SemanticConfig {
        concurrency: config_value,
        ..Default::default()
    };
    resolve_split_semantic_concurrency(cli_override, &config, None, opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nz(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).expect("test value must be non-zero")
    }

    #[test]
    fn cli_override_wins_over_config() {
        let opts = ResolveOptions {
            available_parallelism: Some(nz(32)),
            ..Default::default()
        };
        let config = SemanticConfig {
            parse_concurrency: Some(16),
            embed_concurrency: Some(8),
            ..Default::default()
        };
        let r = resolve_split_semantic_concurrency(Some(2), &config, None, opts);
        assert_eq!(r.parse_threads.get(), 2);
        assert_eq!(r.parse_source, ConcurrencySource::Cli);
        assert_eq!(r.embed_threads.get(), 2);
        assert_eq!(r.embed_source, ConcurrencySource::Cli);
    }

    #[test]
    fn split_field_overrides_legacy() {
        let opts = ResolveOptions {
            available_parallelism: Some(nz(32)),
            ..Default::default()
        };
        let config = SemanticConfig {
            concurrency: Some(6),
            parse_concurrency: Some(12),
            embed_concurrency: Some(3),
            ..Default::default()
        };
        let r = resolve_split_semantic_concurrency(None, &config, None, opts);
        assert_eq!(r.parse_threads.get(), 12);
        assert_eq!(r.parse_source, ConcurrencySource::ConfigParse);
        assert_eq!(r.embed_threads.get(), 3);
        assert_eq!(r.embed_source, ConcurrencySource::ConfigEmbed);
    }

    #[test]
    fn parse_and_embed_resolve_independently() {
        let opts = ResolveOptions {
            available_parallelism: Some(nz(32)),
            ..Default::default()
        };
        let config = SemanticConfig {
            parse_concurrency: Some(24),
            ..Default::default()
        };
        let r = resolve_split_semantic_concurrency(None, &config, None, opts);
        assert_eq!(r.parse_threads.get(), 24);
        assert_eq!(r.parse_source, ConcurrencySource::ConfigParse);
        assert_eq!(r.embed_threads.get(), 4); // DEFAULT_EMBED_CAP
        assert_eq!(r.embed_source, ConcurrencySource::Default);
    }

    #[test]
    fn embed_defaults_to_4_when_unset() {
        let opts = ResolveOptions::default();
        let config = SemanticConfig::default();
        let r = resolve_split_semantic_concurrency(None, &config, None, opts);
        assert_eq!(r.embed_threads.get(), 4);
        assert_eq!(r.embed_source, ConcurrencySource::Default);
    }

    #[test]
    fn custom_embed_cap_is_respected() {
        let opts = ResolveOptions {
            available_parallelism: Some(nz(32)),
            embed_cap: nz(2),
            cap_source: ConcurrencySource::Default,
        };
        let config = SemanticConfig {
            embed_concurrency_cap: Some(16),
            embed_concurrency: Some(8),
            ..Default::default()
        };
        let r = resolve_split_semantic_concurrency(None, &config, None, opts);
        assert_eq!(r.embed_cap.get(), 16);
        assert_eq!(r.embed_threads.get(), 8);
    }

    #[test]
    fn semaphore_try_acquire_logic() {
        let sem = EmbedSemaphore::new(2);
        assert_eq!(sem.available(), 2);
        let p1 = sem.try_acquire().unwrap();
        assert_eq!(sem.available(), 1);
        let p2 = sem.try_acquire().unwrap();
        assert_eq!(sem.available(), 0);
        assert!(sem.try_acquire().is_none());
        drop(p1);
        assert_eq!(sem.available(), 1);
        let p3 = sem.try_acquire().unwrap();
        assert_eq!(sem.available(), 0);
        drop(p2);
        drop(p3);
        assert_eq!(sem.available(), 2);
    }

    #[test]
    fn semaphore_spin_and_noop() {
        let sem = EmbedSemaphore::new(1);
        let _p1 = sem.try_acquire_spin(10).unwrap();
        assert!(sem.try_acquire_spin(10).is_none());
        drop(_p1);

        let p2 = sem.try_acquire_spin(10).unwrap();
        let p_noop = EmbedPermit::noop(&sem);
        assert_eq!(sem.available(), 0);
        drop(p_noop);
        assert_eq!(sem.available(), 0);
        drop(p2);
        assert_eq!(sem.available(), 1);
    }

    #[test]
    fn semaphore_strict_concurrency_cap() {
        use std::sync::Arc;
        use std::thread;

        let sem = Arc::new(EmbedSemaphore::new(2));
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut handles = vec![];

        for _ in 0..20 {
            let sem_clone = sem.clone();
            let active_clone = active.clone();
            let max_active_clone = max_active.clone();
            handles.push(thread::spawn(move || {
                let _permit = sem_clone.acquire();
                let current = active_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

                // Track max active seen
                let mut prev = max_active_clone.load(std::sync::atomic::Ordering::SeqCst);
                while current > prev {
                    match max_active_clone.compare_exchange_weak(
                        prev,
                        current,
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                    ) {
                        Ok(_) => break,
                        Err(actual) => prev = actual,
                    }
                }

                thread::sleep(std::time::Duration::from_millis(2));
                active_clone.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }));
        }

        for h in handles {
            let _ = h.join();
        }

        assert!(max_active.load(std::sync::atomic::Ordering::SeqCst) <= 2);
    }
}
