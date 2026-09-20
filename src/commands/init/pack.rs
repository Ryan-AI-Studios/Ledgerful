use crate::commands::config::edit::{execute_config_set_in_quiet, load_config_doc};
use crate::commands::schedule::{NightlyInstallStatus, install_nightly_if_missing};
use crate::state::layout::Layout;
use miette::{IntoDiagnostic, Result};
use std::io::Write;
use toml_edit::DocumentMut;

const PACK_KEYS: &[(&str, &[&str], &str)] = &[
    (
        "coverage.global",
        &["coverage", "enabled"],
        "coverage.enabled=true",
    ),
    (
        "coverage.services",
        &["coverage", "services", "enabled"],
        "coverage.services.enabled=true",
    ),
    (
        "coverage.deploy",
        &["coverage", "deploy", "enabled"],
        "coverage.deploy.enabled=true",
    ),
    (
        "bridge.enabled",
        &["bridge", "enabled"],
        "bridge.enabled=true",
    ),
];

pub(super) fn apply_operator_pack(layout: &Layout, out: &mut impl Write) -> Result<()> {
    let (doc, _) = load_config_doc(layout)?;
    let mut failed = false;
    let mut lines: Vec<(String, String)> = Vec::new();

    for (id, path, set) in PACK_KEYS {
        if on_disk_true(&doc, path) {
            lines.push(((*id).to_string(), "already set".to_string()));
            continue;
        }
        match execute_config_set_in_quiet(layout, set) {
            Ok(()) => lines.push(((*id).to_string(), "applied".to_string())),
            Err(e) => {
                failed = true;
                lines.push(((*id).to_string(), format!("FAILED {e}")));
            }
        }
    }

    match install_nightly_if_missing(layout) {
        Ok(NightlyInstallStatus::Installed) => {
            lines.push(("nightly".to_string(), "installed".to_string()));
        }
        Ok(NightlyInstallStatus::AlreadyInstalled) => {
            lines.push((
                "nightly".to_string(),
                "skipped (already installed)".to_string(),
            ));
        }
        Ok(NightlyInstallStatus::UnsupportedOs) => {
            lines.push((
                "nightly".to_string(),
                "skipped (unsupported OS)".to_string(),
            ));
        }
        Err(e) => {
            failed = true;
            lines.push(("nightly".to_string(), format!("FAILED {e}")));
        }
    }

    writeln!(out, "Operator pack:").into_diagnostic()?;
    for (id, status) in &lines {
        writeln!(out, "  {id}: {status}").into_diagnostic()?;
    }

    if failed {
        return Err(miette::miette!("operator pack had one or more failures"));
    }
    Ok(())
}

fn on_disk_true(doc: &DocumentMut, path: &[&str]) -> bool {
    let mut current = doc.as_item();
    for key in path {
        current = match current.get(*key) {
            Some(item) => item,
            None => return false,
        };
    }
    current.as_bool() == Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use std::fs;
    use tempfile::tempdir;

    fn harness() -> (tempfile::TempDir, Layout) {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).expect("utf8");
        let state = root.join(".ledgerful");
        let layout = Layout::from_roots(root, &state);
        layout.ensure_state_dir().unwrap();
        (tmp, layout)
    }

    fn write_config(layout: &Layout, body: &str) {
        fs::write(layout.config_file().as_std_path(), body).unwrap();
    }

    #[test]
    #[serial_test::serial(env)]
    fn pack_fills_product_defaults_and_preserves_unrelated_keys() {
        mod env_guard {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/integration/common/env_guard.rs"
            ));
        }
        use env_guard::TempEnv;

        let (_tmp, layout) = harness();
        write_config(
            &layout,
            "[core]\nstrict = false\n\n[hotspots]\nlimit = 3\n\n[bridge]\nenabled = false\n",
        );
        let _seam = TempEnv::set("LEDGERFUL_TEST_NIGHTLY_SEAM", "fake");
        let mut buf = Vec::new();
        apply_operator_pack(&layout, &mut buf).expect("pack");
        let out = String::from_utf8(buf).unwrap();
        assert!(out.starts_with("Operator pack:\n"), "{out}");
        assert!(out.contains("  coverage.global: applied\n"), "{out}");
        assert!(out.contains("  coverage.services: applied\n"), "{out}");
        assert!(out.contains("  coverage.deploy: applied\n"), "{out}");
        assert!(out.contains("  bridge.enabled: applied\n"), "{out}");
        assert!(out.contains("  nightly: installed\n"), "{out}");

        let disk = fs::read_to_string(layout.config_file().as_std_path()).unwrap();
        assert!(disk.contains("limit = 3"), "{disk}");
        let reloaded = crate::config::load::load_config(&layout).unwrap();
        assert!(reloaded.coverage.enabled);
        assert!(reloaded.coverage.services.enabled);
        assert!(reloaded.coverage.deploy.enabled);
        assert!(reloaded.bridge.enabled);
    }

    #[test]
    #[serial_test::serial(env)]
    fn pack_skips_on_disk_true_even_when_bridge_env_is_zero() {
        mod env_guard {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/integration/common/env_guard.rs"
            ));
        }
        use env_guard::TempEnv;

        let (_tmp, layout) = harness();
        write_config(
            &layout,
            "[coverage]\nenabled = true\n[coverage.services]\nenabled = true\n[coverage.deploy]\nenabled = true\n[bridge]\nenabled = true\n",
        );
        let _bridge = TempEnv::set("LEDGERFUL_BRIDGE", "0");
        let _seam = TempEnv::set("LEDGERFUL_TEST_NIGHTLY_SEAM", "skip");
        let mut buf = Vec::new();
        apply_operator_pack(&layout, &mut buf).expect("pack");
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("  coverage.global: already set\n"), "{out}");
        assert!(out.contains("  coverage.services: already set\n"), "{out}");
        assert!(out.contains("  coverage.deploy: already set\n"), "{out}");
        assert!(out.contains("  bridge.enabled: already set\n"), "{out}");
        assert!(
            out.contains("  nightly: skipped (already installed)\n"),
            "{out}"
        );
        let disk = fs::read_to_string(layout.config_file().as_std_path()).unwrap();
        assert!(disk.contains("enabled = true"));
    }

    #[test]
    #[serial_test::serial(env)]
    fn pack_applies_on_disk_false_even_when_bridge_env_is_one() {
        mod env_guard {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/integration/common/env_guard.rs"
            ));
        }
        use env_guard::TempEnv;

        let (_tmp, layout) = harness();
        write_config(&layout, "[bridge]\nenabled = false\n");
        let _bridge = TempEnv::set("LEDGERFUL_BRIDGE", "1");
        let _seam = TempEnv::set("LEDGERFUL_TEST_NIGHTLY_SEAM", "skip");
        let mut buf = Vec::new();
        apply_operator_pack(&layout, &mut buf).expect("pack");
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("  bridge.enabled: applied\n"), "{out}");
        let disk = fs::read_to_string(layout.config_file().as_std_path()).unwrap();
        assert!(disk.contains("enabled = true"), "{disk}");
    }

    #[test]
    #[serial_test::serial(env)]
    #[allow(clippy::permissions_set_readonly_false)]
    fn pack_failed_key_prints_full_block_then_err() {
        mod env_guard {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/integration/common/env_guard.rs"
            ));
        }
        use env_guard::TempEnv;

        let (_tmp, layout) = harness();
        write_config(&layout, "[core]\nstrict = false\n");
        let path = layout.config_file();
        let mut perms = fs::metadata(path.as_std_path()).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(path.as_std_path(), perms).unwrap();
        let _seam = TempEnv::set("LEDGERFUL_TEST_NIGHTLY_SEAM", "skip");
        let mut buf = Vec::new();
        let err = apply_operator_pack(&layout, &mut buf);
        let mut perms = fs::metadata(path.as_std_path()).unwrap().permissions();
        perms.set_readonly(false);
        fs::set_permissions(path.as_std_path(), perms).unwrap();
        assert!(err.is_err(), "{err:?}");
        let out = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.first().copied(), Some("Operator pack:"), "{out}");
        assert!(
            lines
                .get(1)
                .is_some_and(|l| l.starts_with("  coverage.global: FAILED ")),
            "{out}"
        );
        assert!(
            lines
                .get(2)
                .is_some_and(|l| l.starts_with("  coverage.services: FAILED ")),
            "{out}"
        );
        assert!(
            lines
                .get(3)
                .is_some_and(|l| l.starts_with("  coverage.deploy: FAILED ")),
            "{out}"
        );
        assert!(
            lines
                .get(4)
                .is_some_and(|l| l.starts_with("  bridge.enabled: FAILED ")),
            "{out}"
        );
        assert_eq!(
            lines.get(5).copied(),
            Some("  nightly: skipped (already installed)"),
            "{out}"
        );
    }
}
