//! Immediate admission with permits owned by the blocking workers.
use rusqlite::{Connection, InterruptHandle};
use serde::Serialize;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout_at};

const MAX_RESPONSE_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkerError {
    Saturated,
    Deadline,
    Cancelled,
    Join,
    TooLarge,
    Registration,
    Storage,
    PassiveRead(super::pack_store::passive_db::PassiveReadError),
}

/// Serialized response only; contains no connection, statement or native handle.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct DetachedResponse(Box<[u8]>);
impl DetachedResponse {
    pub(crate) fn new(bytes: Vec<u8>) -> Result<Self, WorkerError> {
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(WorkerError::TooLarge);
        }
        Ok(Self(bytes.into_boxed_slice()))
    }
    pub(crate) fn serialize<T: Serialize>(value: &T) -> Result<Self, WorkerError> {
        let mut output = BoundedJson {
            bytes: Vec::with_capacity(MAX_RESPONSE_BYTES),
            exceeded: false,
        };
        serde_json::to_writer(&mut output, value).map_err(|_| {
            if output.exceeded {
                WorkerError::TooLarge
            } else {
                WorkerError::Storage
            }
        })?;
        Self::new(output.bytes)
    }

    pub(crate) fn into_bytes(self) -> Box<[u8]> {
        self.0
    }
}

struct BoundedJson {
    bytes: Vec<u8>,
    exceeded: bool,
}

impl io::Write for BoundedJson {
    fn write(&mut self, contents: &[u8]) -> io::Result<usize> {
        if contents.len() > MAX_RESPONSE_BYTES - self.bytes.len() {
            self.exceeded = true;
            return Err(io::Error::other(
                "change evidence response exceeds its budget",
            ));
        }
        self.bytes.extend_from_slice(contents);
        Ok(contents.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Cancellation {
    flag: AtomicBool,
    handle: Mutex<Option<InterruptHandle>>,
}
impl Cancellation {
    fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
        // The guard ensures close cannot overlap interrupt: the registered connection
        // remains borrowed until this lock is released and deregistration completes.
        let handle = self.handle.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = handle.as_ref() {
            handle.interrupt();
        }
    }
}

pub(crate) struct WorkerContext {
    cancellation: Arc<Cancellation>,
    deadline: Instant,
}
impl WorkerContext {
    pub(crate) fn deadline(&self) -> std::time::Instant {
        self.deadline.into_std()
    }
    pub(crate) fn interrupted(&self) -> &AtomicBool {
        &self.cancellation.flag
    }
    pub(crate) fn register<'a>(
        &'a self,
        connection: &'a mut Connection,
    ) -> Result<RegisteredConnection<'a>, WorkerError> {
        let mut slot = self
            .cancellation
            .handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if self.cancellation.flag.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            return Err(WorkerError::Cancelled);
        }
        if slot.is_some() {
            return Err(WorkerError::Registration);
        }
        // Refuse contention immediately; later busy_timeout or PRAGMA busy_timeout
        // would overwrite this policy and is forbidden while this guard is live.
        connection
            .busy_handler(Some(|_| false))
            .map_err(|_| WorkerError::Storage)?;
        let state = Arc::clone(&self.cancellation);
        let deadline = self.deadline;
        connection
            .progress_handler(
                1,
                Some(move || state.flag.load(Ordering::Acquire) || Instant::now() >= deadline),
            )
            .map_err(|_| WorkerError::Storage)?;
        *slot = Some(connection.get_interrupt_handle());
        Ok(RegisteredConnection {
            context: self,
            connection,
        })
    }
}
pub(crate) struct RegisteredConnection<'a> {
    context: &'a WorkerContext,
    connection: &'a mut Connection,
}
impl RegisteredConnection<'_> {
    pub(crate) fn connection(&self) -> &Connection {
        self.connection
    }
}
impl Drop for RegisteredConnection<'_> {
    fn drop(&mut self) {
        let mut slot = self
            .context
            .cancellation
            .handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *slot = None;
        // The connection remains exclusively borrowed until deregistration returns.
        // A failure leaves the callback owned by the connection until it closes.
        let _ = self.connection.progress_handler(0, None::<fn() -> bool>);
    }
}

#[derive(Debug)]
pub(crate) struct WorkerGate {
    permits: Arc<Semaphore>,
}

impl WorkerGate {
    pub(crate) fn new() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(2)),
        }
    }

    pub(crate) fn try_start(
        &self,
        deadline: Instant,
        work: impl FnOnce(WorkerContext) -> Result<DetachedResponse, WorkerError> + Send + 'static,
    ) -> Result<RunningWorker, WorkerError> {
        if Instant::now() >= deadline {
            return Err(WorkerError::Deadline);
        }
        let permit = Arc::clone(&self.permits)
            .try_acquire_owned()
            .map_err(|_| WorkerError::Saturated)?;
        let cancellation = Arc::new(Cancellation {
            flag: AtomicBool::new(false),
            handle: Mutex::new(None),
        });
        let worker_cancellation = Arc::clone(&cancellation);
        let join = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let operation = work;
            if Instant::now() >= deadline {
                return Err(WorkerError::Deadline);
            }
            if worker_cancellation.flag.load(Ordering::Acquire) {
                return Err(WorkerError::Cancelled);
            }
            operation(WorkerContext {
                cancellation: worker_cancellation,
                deadline,
            })
        });
        Ok(RunningWorker {
            join,
            cancellation,
            deadline,
            joined: false,
        })
    }
}

pub(crate) struct RunningWorker {
    join: JoinHandle<Result<DetachedResponse, WorkerError>>,
    cancellation: Arc<Cancellation>,
    deadline: Instant,
    joined: bool,
}

impl RunningWorker {
    pub(crate) async fn wait(mut self) -> Result<DetachedResponse, WorkerError> {
        let joined = timeout_at(self.deadline, &mut self.join)
            .await
            .map_err(|_| WorkerError::Deadline)?;
        if Instant::now() >= self.deadline {
            return Err(WorkerError::Deadline);
        }
        self.joined = true;
        joined.map_err(|_| WorkerError::Join)?
    }
}

impl Drop for RunningWorker {
    fn drop(&mut self) {
        if !self.joined {
            self.cancellation.cancel();
        }
    }
}

#[cfg(test)]
#[test]
fn serialization_checks_escaped_byte_length_and_returns_only_complete_json() {
    let exact = "a".repeat(MAX_RESPONSE_BYTES - 2);
    let body = DetachedResponse::serialize(&exact).unwrap().into_bytes();
    assert_eq!(body.len(), MAX_RESPONSE_BYTES);
    assert_eq!(serde_json::from_slice::<String>(&body).unwrap(), exact);

    let escaped = char::from(0).to_string().repeat(MAX_RESPONSE_BYTES / 6 + 1);
    assert!(escaped.len() < MAX_RESPONSE_BYTES);
    assert_eq!(
        DetachedResponse::serialize(&escaped),
        Err(WorkerError::TooLarge)
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::oneshot;
    #[test]
    fn cancellation_interrupts_only_registered_connection() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let gate = WorkerGate::new();
            let (started, ready) = oneshot::channel();
            let (finished, receipt) = oneshot::channel();
            let worker = gate.try_start(Instant::now() + Duration::from_secs(5), move |ctx| {
                let mut db = Connection::open_in_memory().unwrap();
                let registered = ctx.register(&mut db).unwrap();
                started.send(()).unwrap();
                let result = registered.connection().query_row("WITH RECURSIVE x(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM x WHERE n<1000000000) SELECT sum(n) FROM x", [], |r| r.get::<_, i64>(0));
                finished.send(matches!(result, Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::OperationInterrupted)).unwrap();
                DetachedResponse::new(vec![])
            }).unwrap();
            ready.await.unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
            drop(worker);
            assert!(tokio::time::timeout(Duration::from_secs(2), receipt).await.unwrap().unwrap());
            let other = Connection::open_in_memory().unwrap();
            assert_eq!(other.query_row("SELECT 7", [], |r| r.get::<_, i64>(0)).unwrap(), 7);
        });
    }
    #[test]
    fn cancellation_before_query_is_seen_by_progress_handler() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let gate = WorkerGate::new();
            let (entered, entered_rx) = oneshot::channel();
            let (release, hold) = std::sync::mpsc::channel();
            let (done, done_rx) = oneshot::channel();
            let worker = gate.try_start(Instant::now() + Duration::from_secs(5), move |ctx| {
                let mut db = Connection::open_in_memory().unwrap();
                let registered = ctx.register(&mut db).unwrap();
                entered.send(()).unwrap();
                hold.recv_timeout(Duration::from_secs(3)).unwrap();
                let result = registered.connection().query_row("WITH RECURSIVE x(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM x WHERE n<1000000000) SELECT sum(n) FROM x", [], |r| r.get::<_, i64>(0));
                done.send(matches!(result, Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::OperationInterrupted)).unwrap();
                DetachedResponse::new(vec![])
            }).unwrap();
            entered_rx.await.unwrap();
            drop(worker);
            release.send(()).unwrap();
            assert!(tokio::time::timeout(Duration::from_secs(2), done_rx).await.unwrap().unwrap());
        });
    }
    #[test]
    fn busy_connection_refuses_without_waiting() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("worker-proof.sqlite");
        let owner = Connection::open(&path).unwrap();
        owner
            .execute_batch("CREATE TABLE t (x INTEGER); BEGIN EXCLUSIVE; INSERT INTO t VALUES (1);")
            .unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let gate = WorkerGate::new();
            let worker = gate.try_start(Instant::now() + Duration::from_secs(2), move |ctx| {
                let mut db = Connection::open(&path).unwrap();
                let registered = ctx.register(&mut db).unwrap();
                let start = std::time::Instant::now();
                let result = registered.connection().query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0));
                assert!(matches!(result, Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::DatabaseBusy));
                assert!(start.elapsed() < Duration::from_secs(1));
                DetachedResponse::new(vec![])
            }).unwrap();
            assert!(worker.wait().await.is_ok());
        });
        owner.execute_batch("ROLLBACK").unwrap();
        drop(owner);
    }
    #[test]
    fn response_limit_is_enforced() {
        assert!(DetachedResponse::new(vec![0; 256 * 1024]).is_ok());
        assert_eq!(
            DetachedResponse::new(vec![0; 256 * 1024 + 1]),
            Err(WorkerError::TooLarge)
        );
    }
}

#[cfg(test)]
mod strict_tests {
    use super::*;
    use std::future::{Future, poll_fn};
    use std::sync::mpsc;
    use std::task::Poll;
    use std::time::Duration;
    use tokio::sync::oneshot;
    use tokio::time::{sleep, sleep_until, timeout};

    fn response(n: u8) -> Result<DetachedResponse, WorkerError> {
        DetachedResponse::new(vec![n])
    }

    const TEST_LIMIT: Duration = Duration::from_secs(3);
    const SHORT_DEADLINE: Duration = Duration::from_millis(250);

    struct LatchedWorker {
        worker: RunningWorker,
        started: oneshot::Receiver<Arc<Cancellation>>,
        release: mpsc::Sender<()>,
        observed_cancel: oneshot::Receiver<bool>,
    }

    fn latched(gate: &WorkerGate, deadline: Instant, result: u8) -> LatchedWorker {
        let (started, receipt) = oneshot::channel();
        let (observed, observed_cancel) = oneshot::channel();
        let (release, hold) = mpsc::channel();
        let worker = gate
            .try_start(deadline, move |ctx| {
                let interrupted = Arc::clone(&ctx.cancellation);
                assert!(started.send(Arc::clone(&interrupted)).is_ok());
                hold.recv_timeout(TEST_LIMIT).unwrap();
                observed
                    .send(interrupted.flag.load(Ordering::Acquire))
                    .unwrap();
                response(result)
            })
            .unwrap();
        LatchedWorker {
            worker,
            started: receipt,
            release,
            observed_cancel,
        }
    }

    async fn permits_recovered(gate: &WorkerGate, expected: usize) {
        timeout(TEST_LIMIT, async {
            while gate.permits.available_permits() != expected {
                sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn timeout_and_dropped_wait_keep_both_slots_until_the_closures_exit() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let gate = WorkerGate::new();
            let timed = latched(&gate, Instant::now() + SHORT_DEADLINE, 1);
            let dropped = latched(&gate, Instant::now() + TEST_LIMIT, 2);
            let timed_flag = timeout(TEST_LIMIT, timed.started).await.unwrap().unwrap();
            let dropped_flag = timeout(TEST_LIMIT, dropped.started).await.unwrap().unwrap();

            assert_eq!(timed.worker.wait().await, Err(WorkerError::Deadline));
            assert!(timed_flag.flag.load(Ordering::Acquire));
            assert!(!dropped_flag.flag.load(Ordering::Acquire));

            let mut waiting = Box::pin(dropped.worker.wait());
            poll_fn(|cx| {
                assert!(waiting.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            drop(waiting);
            assert!(dropped_flag.flag.load(Ordering::Acquire));
            assert!(matches!(
                gate.try_start(Instant::now() + TEST_LIMIT, |_| response(3)),
                Err(WorkerError::Saturated)
            ));

            timed.release.send(()).unwrap();
            assert!(
                timeout(TEST_LIMIT, timed.observed_cancel)
                    .await
                    .unwrap()
                    .unwrap()
            );
            permits_recovered(&gate, 1).await;
            let summary = gate
                .try_start(Instant::now() + TEST_LIMIT, |ctx| {
                    assert!(!ctx.interrupted().load(Ordering::Acquire));
                    response(4)
                })
                .unwrap()
                .wait()
                .await;
            assert_eq!(summary, response(4));
            assert_eq!(gate.permits.available_permits(), 1);

            dropped.release.send(()).unwrap();
            assert!(
                timeout(TEST_LIMIT, dropped.observed_cancel)
                    .await
                    .unwrap()
                    .unwrap()
            );
            permits_recovered(&gate, 2).await;
        });
    }

    #[test]
    fn pre_expired_request_keeps_both_slots_available() {
        let gate = WorkerGate::new();
        let expired = Instant::now() - Duration::from_millis(1);
        assert!(matches!(
            gate.try_start(expired, |_| response(1)),
            Err(WorkerError::Deadline)
        ));
        assert_eq!(gate.permits.available_permits(), 2);
    }

    #[test]
    fn a_ready_worker_result_is_refused_after_its_deadline() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let gate = WorkerGate::new();
            let deadline = Instant::now() + SHORT_DEADLINE;
            let (processed, receipt) = oneshot::channel();
            let worker = gate
                .try_start(deadline, move |_| {
                    processed.send(()).unwrap();
                    response(7)
                })
                .unwrap();
            timeout(TEST_LIMIT, receipt).await.unwrap().unwrap();
            timeout(TEST_LIMIT, async {
                while !worker.join.is_finished() {
                    sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            assert!(Instant::now() < deadline);
            sleep_until(deadline).await;

            assert_eq!(worker.wait().await, Err(WorkerError::Deadline));
            assert_eq!(gate.permits.available_permits(), 2);
        });
    }

    #[test]
    fn a_worker_that_starts_after_its_deadline_skips_the_operation() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let gate = WorkerGate::new();
            let (occupied, ready) = oneshot::channel();
            let (release, hold) = mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                occupied.send(()).unwrap();
                hold.recv_timeout(TEST_LIMIT).unwrap();
            });
            timeout(TEST_LIMIT, ready).await.unwrap().unwrap();

            let attempted = Arc::new(AtomicBool::new(false));
            let worker_attempted = Arc::clone(&attempted);
            let deadline = Instant::now() + Duration::from_millis(30);
            let worker = gate
                .try_start(deadline, move |_| {
                    worker_attempted.store(true, Ordering::Release);
                    response(8)
                })
                .unwrap();
            sleep_until(deadline).await;
            assert_eq!(gate.permits.available_permits(), 1);
            release.send(()).unwrap();
            timeout(TEST_LIMIT, blocker).await.unwrap().unwrap();
            permits_recovered(&gate, 2).await;

            assert!(!attempted.load(Ordering::Acquire));
            assert_eq!(worker.wait().await, Err(WorkerError::Deadline));
        });
    }
}
