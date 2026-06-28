use super::ParsedCIGate;

pub fn workflow_name_from_filename(filename: &str) -> String {
    let stem = std::path::Path::new(filename)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| filename.to_string());
    if stem.is_empty() {
        filename.to_string()
    } else {
        stem
    }
}

pub fn parse_github_actions(content: &str, filename: Option<&str>) -> Vec<ParsedCIGate> {
    let mut gates = Vec::new();
    let mut triggers = Vec::new();
    let mut workflow_name: Option<String> = None;
    let mut in_jobs_section = false;

    // Extract triggers from the "on:" section
    let mut in_on_section = false;
    let mut on_indent = 0;
    let mut brace_depth = 0;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('#') {
            continue;
        }

        let line_indent = line.len() - line.trim_start().len();

        if (trimmed == "jobs:" || trimmed.starts_with("jobs:")) && line_indent == 0 {
            in_jobs_section = true;
        }

        // Detect workflow name only at top level, before the jobs section
        if trimmed.starts_with("name:") && !in_jobs_section && line_indent == 0 {
            workflow_name = Some(
                trimmed
                    .strip_prefix("name:")
                    .unwrap_or(trimmed)
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string(),
            );
        }

        // Detect "on:" key at top level

        if trimmed == "on:" || trimmed.starts_with("on:") {
            in_on_section = true;
            on_indent = line_indent;
            brace_depth = 0;
            // Check for inline triggers like: on: push
            if trimmed != "on:" {
                let rest = trimmed.strip_prefix("on:").unwrap_or("").trim();
                if !rest.is_empty() && !rest.contains('{') && !rest.contains(':') {
                    triggers.push(rest.to_string());
                }
            }
            continue;
        }

        if in_on_section {
            // Check if we've left the "on:" section
            if !trimmed.is_empty() && line_indent <= on_indent && !trimmed.starts_with('-') {
                in_on_section = false;
            } else {
                if trimmed.contains('{') {
                    brace_depth += trimmed.matches('{').count();
                }
                if trimmed.contains('}') {
                    brace_depth = brace_depth.saturating_sub(trimmed.matches('}').count());
                }

                if brace_depth == 0 {
                    if let Some(key) = trimmed.strip_suffix(':')
                        && !key.starts_with('#')
                        && !key.contains(' ')
                        && key != "branches"
                        && key != "paths"
                        && key != "tags"
                    {
                        triggers.push(key.to_string());
                    }
                    if trimmed.starts_with('-') {
                        let item = trimmed.strip_prefix('-').unwrap_or(trimmed).trim();
                        let item = item.strip_suffix(',').unwrap_or(item).trim();
                        if !item.is_empty() && !item.starts_with('#') {
                            triggers.push(item.to_string());
                        }
                    }
                }
            }
        }
    }

    let trigger_str = if triggers.is_empty() {
        None
    } else {
        Some(triggers.join(", "))
    };

    let fallback_name = filename.map(workflow_name_from_filename);
    let workflow_name = workflow_name.or(fallback_name);

    // Extract jobs
    let mut current_job: Option<String> = None;
    let mut current_job_steps: Vec<String> = Vec::new();
    let mut current_job_env: Option<String> = None;
    let mut current_job_artifacts: Vec<String> = Vec::new();
    let mut current_job_releases: Vec<String> = Vec::new();
    let mut in_steps = false;
    let mut jobs_indent = 0;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }

        let line_indent = line.len() - line.trim_start().len();

        if trimmed == "jobs:" || trimmed.starts_with("jobs:") {
            in_jobs_section = true;
            jobs_indent = line_indent;
            continue;
        }

        if in_jobs_section {
            if !trimmed.is_empty() && line_indent <= jobs_indent {
                in_jobs_section = false;
                if let Some(job_name) = current_job.take() {
                    gates.push(ParsedCIGate {
                        job_name,
                        trigger: trigger_str.clone(),
                        steps: if current_job_steps.is_empty() {
                            None
                        } else {
                            Some(current_job_steps.join("; "))
                        },
                        workflow_name: workflow_name.clone(),
                        environment: current_job_env.take(),
                        artifacts: if current_job_artifacts.is_empty() {
                            None
                        } else {
                            Some(current_job_artifacts.clone())
                        },
                        release_gates: if current_job_releases.is_empty() {
                            None
                        } else {
                            Some(current_job_releases.clone())
                        },
                    });
                    current_job_steps.clear();
                    current_job_artifacts.clear();
                    current_job_releases.clear();
                }
                continue;
            }

            if line_indent == jobs_indent + 2 && trimmed.ends_with(':') && !trimmed.starts_with('-')
            {
                if let Some(job_name) = current_job.take() {
                    gates.push(ParsedCIGate {
                        job_name,
                        trigger: trigger_str.clone(),
                        steps: if current_job_steps.is_empty() {
                            None
                        } else {
                            Some(current_job_steps.join("; "))
                        },
                        workflow_name: workflow_name.clone(),
                        environment: current_job_env.take(),
                        artifacts: if current_job_artifacts.is_empty() {
                            None
                        } else {
                            Some(current_job_artifacts.clone())
                        },
                        release_gates: if current_job_releases.is_empty() {
                            None
                        } else {
                            Some(current_job_releases.clone())
                        },
                    });
                    current_job_steps.clear();
                    current_job_artifacts.clear();
                    current_job_releases.clear();
                }
                current_job = Some(trimmed.strip_suffix(':').unwrap_or(trimmed).to_string());
                in_steps = false;
                continue;
            }

            if current_job.is_some() {
                if trimmed.starts_with("environment:") {
                    let env = trimmed
                        .strip_prefix("environment:")
                        .unwrap_or(trimmed)
                        .trim();
                    if !env.is_empty() {
                        current_job_env =
                            Some(env.trim_matches('"').trim_matches('\'').to_string());
                    }
                }

                if trimmed == "steps:" || trimmed.starts_with("steps:") {
                    in_steps = true;
                    continue;
                }

                if in_steps && line_indent > 0 {
                    if trimmed.starts_with("- name:") {
                        let name = trimmed
                            .strip_prefix("- name:")
                            .unwrap_or(trimmed)
                            .trim()
                            .trim_matches('"')
                            .trim_matches('\'')
                            .to_string();
                        if !name.is_empty() {
                            current_job_steps.push(name);
                        }
                    } else if trimmed.starts_with("- run:") {
                        let cmd = trimmed
                            .strip_prefix("- run:")
                            .unwrap_or(trimmed)
                            .trim()
                            .trim_matches('"')
                            .trim_matches('\'')
                            .to_string();
                        if !cmd.is_empty() {
                            current_job_steps.push(cmd);
                        }
                    } else if trimmed.contains("uses: actions/upload-artifact") {
                        current_job_artifacts.push("upload-artifact".to_string());
                    } else if trimmed.contains("uses: softprops/action-gh-release") {
                        current_job_releases.push("gh-release".to_string());
                    }
                }
            }
        }
    }

    if let Some(job_name) = current_job.take() {
        gates.push(ParsedCIGate {
            job_name,
            trigger: trigger_str.clone(),
            steps: if current_job_steps.is_empty() {
                None
            } else {
                Some(current_job_steps.join("; "))
            },
            workflow_name: workflow_name.clone(),
            environment: current_job_env,
            artifacts: if current_job_artifacts.is_empty() {
                None
            } else {
                Some(current_job_artifacts)
            },
            release_gates: if current_job_releases.is_empty() {
                None
            } else {
                Some(current_job_releases)
            },
        });
    }

    gates
}
