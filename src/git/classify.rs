use crate::git::{ChangeType, FileChange};
use gix::Repository;
use gix::bstr::ByteSlice;

pub fn classify_status(_repo: &Repository, item: &gix::status::Item) -> Option<Vec<FileChange>> {
    let mut results = Vec::new();

    match item {
        gix::status::Item::IndexWorktree(change) => {
            let (path, change_type) = match change {
                gix::status::index_worktree::Item::Rewrite {
                    source,
                    dirwalk_entry,
                    copy,
                    ..
                } => {
                    let path =
                        gix::path::from_bstr(dirwalk_entry.rela_path.as_bstr()).to_path_buf();
                    let change_type = if *copy {
                        ChangeType::Added
                    } else {
                        ChangeType::Renamed {
                            old_path: gix::path::from_bstr(source.rela_path()).to_path_buf(),
                        }
                    };
                    (path, change_type)
                }
                _ => {
                    let summary = change.summary()?;
                    let path = gix::path::from_bstr(change.rela_path()).to_path_buf();
                    let change_type = match summary {
                        gix::status::index_worktree::iter::Summary::Added => ChangeType::Added,
                        gix::status::index_worktree::iter::Summary::Modified => {
                            ChangeType::Modified
                        }
                        gix::status::index_worktree::iter::Summary::Removed => ChangeType::Deleted,
                        gix::status::index_worktree::iter::Summary::TypeChange => {
                            ChangeType::Modified
                        }
                        gix::status::index_worktree::iter::Summary::Copied => ChangeType::Added,
                        gix::status::index_worktree::iter::Summary::IntentToAdd => {
                            ChangeType::Added
                        }
                        gix::status::index_worktree::iter::Summary::Conflict => {
                            ChangeType::Modified
                        }
                        gix::status::index_worktree::iter::Summary::Renamed => {
                            unreachable!("rewrite variants are handled above")
                        }
                    };
                    (path, change_type)
                }
            };

            results.push(FileChange {
                path,
                change_type,
                is_staged: false,
            });
        }
        gix::status::Item::TreeIndex(change) => {
            use gix::diff::index::Change;
            let (change_type, path) = match change {
                Change::Addition { location, .. } => (
                    ChangeType::Added,
                    gix::path::from_bstr(location.as_ref()).to_path_buf(),
                ),
                Change::Deletion { location, .. } => (
                    ChangeType::Deleted,
                    gix::path::from_bstr(location.as_ref()).to_path_buf(),
                ),
                Change::Modification { location, .. } => (
                    ChangeType::Modified,
                    gix::path::from_bstr(location.as_ref()).to_path_buf(),
                ),
                Change::Rewrite {
                    source_location,
                    location,
                    copy: false,
                    ..
                } => (
                    ChangeType::Renamed {
                        old_path: gix::path::from_bstr(source_location.as_ref()).to_path_buf(),
                    },
                    gix::path::from_bstr(location.as_ref()).to_path_buf(),
                ),
                Change::Rewrite {
                    location,
                    copy: true,
                    ..
                } => (
                    ChangeType::Added,
                    gix::path::from_bstr(location.as_ref()).to_path_buf(),
                ),
            };

            results.push(FileChange {
                path,
                change_type,
                is_staged: true,
            });
        }
    }

    if results.is_empty() {
        None
    } else {
        Some(results)
    }
}
