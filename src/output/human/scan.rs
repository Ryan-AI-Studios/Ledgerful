use crate::output::table::{apply_table_style, resolve_table_style};
use comfy_table::{Cell, Table};
use owo_colors::{OwoColorize, Stream, Style};

pub fn print_scan_summary(snapshot: &crate::git::RepoSnapshot) {
    println!(
        "\n{}",
        "Ledgerful Git Scan Summary"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().underline()))
    );
    println!(
        "{:<15} {}",
        "Branch:".if_supports_color(Stream::Stdout, |s| s.bold()),
        snapshot.branch_name.as_deref().unwrap_or("unknown")
    );
    println!(
        "{:<15} {}",
        "HEAD:".if_supports_color(Stream::Stdout, |s| s.bold()),
        snapshot.head_hash.as_deref().unwrap_or("unknown")
    );

    let state_str = if snapshot.is_clean {
        "CLEAN"
            .if_supports_color(Stream::Stdout, |s| s.green())
            .to_string()
    } else {
        "DIRTY"
            .if_supports_color(Stream::Stdout, |s| s.yellow())
            .to_string()
    };
    println!(
        "{:<15} {}",
        "State:".if_supports_color(Stream::Stdout, |s| s.bold()),
        state_str
    );

    if !snapshot.changes.is_empty() {
        // Prefer shared state (linked worktrees). Non-git → Layout::new(cwd) for
        // ignore-pattern config only — no DB open. Resolve-after-discover fails closed.
        let layout = match crate::commands::helpers::get_layout_or_cwd_if_not_git() {
            Ok(l) => l,
            Err(_) => {
                // Rare: bad UTF-8 cwd. Use empty defaults without inventing state.
                let root = camino::Utf8PathBuf::from(".");
                crate::state::layout::Layout::new(root)
            }
        };
        let config = crate::config::load::load_config(&layout).unwrap_or_default();
        let ignore_set = if !config.watch.ignore_patterns.is_empty() {
            let mut builder = globset::GlobSetBuilder::new();
            for pattern in &config.watch.ignore_patterns {
                if let Ok(glob) = globset::Glob::new(pattern) {
                    builder.add(glob);
                }
            }
            builder.build().ok()
        } else {
            None
        };

        let mut table = Table::new();
        apply_table_style(&mut table, resolve_table_style());
        table.set_header(vec!["State", "Action", "File Path"]);

        for change in &snapshot.changes {
            let state = if change.is_staged {
                "Staged"
                    .if_supports_color(Stream::Stdout, |s| s.green())
                    .to_string()
            } else {
                "Unstaged"
                    .if_supports_color(Stream::Stdout, |s| s.dimmed())
                    .to_string()
            };
            let action = match &change.change_type {
                crate::git::ChangeType::Added => "Added"
                    .if_supports_color(Stream::Stdout, |s| s.green())
                    .to_string(),
                crate::git::ChangeType::Modified => "Modified"
                    .if_supports_color(Stream::Stdout, |s| s.yellow())
                    .to_string(),
                crate::git::ChangeType::Deleted => "Deleted"
                    .if_supports_color(Stream::Stdout, |s| s.red())
                    .to_string(),
                crate::git::ChangeType::Renamed { old_path } => {
                    format!("Renamed ({})", old_path.display())
                        .if_supports_color(Stream::Stdout, |s| s.blue())
                        .to_string()
                }
            };

            let is_ignored = if let Some(ref set) = ignore_set {
                let path_str = change.path.to_string_lossy().replace('\\', "/");
                set.is_match(path_str)
            } else {
                false
            };

            let path_display = if is_ignored {
                format!("{} (ignored)", change.path.display())
                    .if_supports_color(Stream::Stdout, |s| s.dimmed())
                    .to_string()
            } else {
                change.path.display().to_string()
            };

            table.add_row(vec![
                Cell::new(state),
                Cell::new(action),
                Cell::new(path_display),
            ]);
        }
        println!("{table}");
    }
}
