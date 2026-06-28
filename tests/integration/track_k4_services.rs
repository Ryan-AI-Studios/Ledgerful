use crate::common::DirGuard;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_monorepo_service_impact() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    fs::create_dir_all(root.join("inventory_service/src")).unwrap();
    fs::create_dir_all(root.join("order_service/src")).unwrap();

    fs::write(
        root.join("inventory_service/Cargo.toml"),
        "[package]\nname = \"InventoryService\"\nversion=\"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        root.join("inventory_service/src/main.rs"),
        r#"
        use axum::{routing::get, Router};
        async fn get_inventory() {}
        fn app() -> Router {
            Router::new().route("/api/inventory", get(get_inventory))
        }
    "#,
    )
    .unwrap();

    fs::write(
        root.join("order_service/Cargo.toml"),
        "[package]\nname = \"OrderService\"\nversion=\"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        root.join("order_service/src/main.rs"),
        r#"
        fn check_stock() {
            let res = reqwest::get("/api/inventory");
        }
    "#,
    )
    .unwrap();

    // Init repo and create a git commit so scan can work properly
    std::process::Command::new("git")
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .arg("add")
        .arg(".")
        .current_dir(root)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .arg("commit")
        .arg("-m")
        .arg("Initial")
        .current_dir(root)
        .output()
        .unwrap();

    let _guard = DirGuard::new(root);

    ledgerful::commands::init::execute_init(false).unwrap();

    fs::write(
        root.join(".ledgerful/config.toml"),
        "[coverage]\nenabled = true\n[coverage.services]\nenabled = true\n",
    )
    .unwrap();

    ledgerful::commands::index::execute_index(ledgerful::commands::index::IndexArgs::default())
        .unwrap();

    fs::write(
        root.join("inventory_service/src/main.rs"),
        r#"
        use axum::{routing::get, Router};
        async fn get_inventory_v2() {}
        fn app() -> Router {
            Router::new().route("/api/inventory", get(get_inventory_v2))
        }
    "#,
    )
    .unwrap();

    let packet = ledgerful::commands::impact::execute_impact_silent().unwrap();
    let storage = ledgerful::state::storage::StorageManager::init(
        root.join(".ledgerful/state/ledger.db").as_path(),
    )
    .unwrap();
    if let Some(cozo) = storage.cozo.as_ref() {
        println!(
            "service_roots: {:?}",
            cozo.run_script("?[name, dir_path] := *service_roots{name, dir_path}")
        );
        println!("service_dependencies: {:?}", cozo.run_script("?[caller, callee, p] := *service_dependencies{caller_service: caller, callee_service: callee, pattern: p}"));
    }
    println!("{:#?}", packet);

    let mut has_warning = false;
    for change in packet.changes {
        for warning in change.analysis_warnings {
            if warning.contains("Downstream Breakage") {
                has_warning = true;
            }
        }
    }

    assert!(has_warning, "Should flag downstream breakage");
}
