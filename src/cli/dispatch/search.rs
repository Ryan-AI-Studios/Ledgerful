use crate::cli::args::SearchCliArgs;
use crate::commands::search::SearchArgs;
use miette::Result;

pub(super) fn dispatch_search(current_dir: std::path::PathBuf, args: SearchCliArgs) -> Result<()> {
    use crate::commands::search::SearchJsonMode;
    let SearchCliArgs {
        query,
        regex,
        semantic,
        limit,
        index,
        json,
        json_lines,
        auto_index,
        hybrid,
    } = args;
    let query = query.join(" ");
    let json_mode = if json {
        SearchJsonMode::Envelope
    } else if json_lines {
        SearchJsonMode::Lines
    } else {
        SearchJsonMode::Off
    };
    let project_id = current_dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    crate::commands::search::execute_search(SearchArgs {
        query,
        regex,
        semantic,
        limit,
        index,
        json_mode,
        auto_index,
        project_id,
        hybrid,
    })
}
