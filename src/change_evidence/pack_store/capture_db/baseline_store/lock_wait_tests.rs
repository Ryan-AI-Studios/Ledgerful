use super::{BaselineCheckout, CaptureDbUnavailable, Layout, Observation, StateDirOrigin};
use crate::change_evidence::{buffer::MemoryBudget, git::exact_oid};
use rusqlite::{Connection, TransactionBehavior};
use std::{
    path::Path,
    process::Command,
    sync::{atomic::AtomicBool, mpsc},
    thread,
    time::{Duration, Instant},
};

const WAIT: Duration = Duration::from_secs(5);
const CONFIG: &[u8] = b"[index]\nstale_threshold_days = 7\n";
const NEW_CONFIG: &[u8] = b"[index]\nstale_threshold_days = 8\n";

fn git(root: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("missing-fixture-config"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_AUTHOR_NAME", "Fixture Operator")
        .env("GIT_COMMITTER_NAME", "Fixture Operator")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .args(args)
        .output()
        .unwrap();
    assert!(result.status.success());
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}

fn recheck_after_real_lock_wait(move_head: bool) {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(windows)]
    let fixture = dunce::canonicalize(dir.path()).expect("canonicalize private fixture");
    #[cfg(not(windows))]
    let fixture = dir.path();
    let root = fixture.join("fixture");
    let template = fixture.join("empty");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&template).unwrap();
    git(
        &root,
        &[
            "init",
            "--quiet",
            "--initial-branch=main",
            "--object-format=sha1",
            &format!("--template={}", template.display()),
        ],
    );
    git(
        &root,
        &["commit", "--quiet", "--allow-empty", "-m", "first"],
    );
    let earlier = git(&root, &["rev-parse", "HEAD"]);
    git(
        &root,
        &["commit", "--quiet", "--allow-empty", "-m", "second"],
    );
    let head = exact_oid(git(&root, &["rev-parse", "HEAD"]).as_bytes()).unwrap();
    let root = camino::Utf8PathBuf::from_path_buf(root).unwrap();
    let layout = Layout::new(&root);
    std::fs::create_dir(layout.state_dir.as_std_path()).unwrap();
    std::fs::write(layout.config_file().as_std_path(), CONFIG).unwrap();

    let io_limit = 4 * 1024 * 1024;
    let memory = MemoryBudget::new(io_limit);
    let interrupted = AtomicBool::new(false);
    let origin = StateDirOrigin::RepositoryLocal;
    let mut observation = Observation::open_scoped(
        &layout,
        origin,
        Instant::now() + 3 * WAIT,
        &interrupted,
        io_limit,
        io_limit,
        &memory,
    )
    .unwrap();
    let guard = observation
        .confirm_scoped_guard(&layout, origin, head)
        .unwrap();
    let checkout = BaselineCheckout {
        layout: &layout,
        origin,
        guard: &guard,
        project_config: blake3::hash(CONFIG),
    };

    let db = fixture.join("observations.db");
    let mut writer = Connection::open(&db).unwrap();
    writer
        .execute("CREATE TABLE unpublished (value INTEGER NOT NULL)", [])
        .unwrap();
    let (locked_tx, locked_rx) = mpsc::sync_channel(1);
    let (wait_tx, wait_rx) = mpsc::sync_channel(1);
    let (released_tx, released_rx) = mpsc::sync_channel(1);
    let changed = if move_head {
        root.join(".git/HEAD")
    } else {
        layout.config_file()
    };
    let new_bytes = if move_head {
        format!("{earlier}\n").into_bytes()
    } else {
        NEW_CONFIG.to_vec()
    };
    let blocker = thread::spawn(move || {
        let blocker = Connection::open(db).unwrap();
        blocker.execute("BEGIN IMMEDIATE", []).unwrap();
        locked_tx.send(()).unwrap();
        wait_rx.recv_timeout(WAIT).unwrap();
        std::fs::write(changed.as_std_path(), new_bytes).unwrap();
        blocker.execute("ROLLBACK", []).unwrap();
        released_tx.send(()).unwrap();
    });
    locked_rx.recv_timeout(WAIT).unwrap();
    writer
        .busy_handler(Some(move |attempt| {
            attempt == 0 && wait_tx.send(()).is_ok() && released_rx.recv_timeout(WAIT).is_ok()
        }))
        .unwrap();

    // This passed with the competing SQLite writer already holding its lock.
    checkout.confirm(&mut observation, head).unwrap();
    let tx = writer
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let _ = tx
        .execute("INSERT INTO unpublished VALUES (1)", [])
        .unwrap();
    let expected = if move_head {
        CaptureDbUnavailable::RevisionChanged
    } else {
        CaptureDbUnavailable::Scope
    };
    // The production callback invokes the same check inside the acquired transaction.
    assert_eq!(checkout.confirm(&mut observation, head), Err(expected));
    drop(tx);
    blocker.join().unwrap();
    assert_eq!(
        writer
            .query_row("SELECT COUNT(*) FROM unpublished", [], |row| row
                .get::<_, u32>(0))
            .unwrap(),
        0
    );
}

#[test]
fn immediate_transaction_rechecks_checkout_after_a_real_sqlite_writer_wait() {
    recheck_after_real_lock_wait(true);
    recheck_after_real_lock_wait(false);
}
