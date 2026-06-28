use super::ParsedCIGate;

pub fn parse_gitlab_ci(content: &str) -> Vec<ParsedCIGate> {
    let mut gates = Vec::new();
    let mut stages: Vec<String> = Vec::new();

    // Parse stages
    let mut in_stages = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("stages:") {
            in_stages = true;
            if let Some(rest) = trimmed.strip_prefix("stages:") {
                let rest = rest.trim();
                if rest.starts_with('[') {
                    let inner = rest.trim_start_matches('[').trim_end_matches(']');
                    for stage in inner.split(',') {
                        let s = stage.trim().trim_matches('"').trim_matches('\'');
                        if !s.is_empty() {
                            stages.push(s.to_string());
                        }
                    }
                    in_stages = false;
                }
            }
            continue;
        }
        if in_stages {
            if trimmed.starts_with('-') {
                let item = trimmed
                    .strip_prefix('-')
                    .unwrap()
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'');
                if !item.is_empty() && !item.contains(':') {
                    stages.push(item.to_string());
                }
            } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
                in_stages = false;
            }
        }
    }

    let mut current_job: Option<String> = None;
    let mut current_steps: Vec<String> = Vec::new();
    let mut current_trigger: Option<String> = None;
    let top_level_keys: std::collections::HashSet<&str> = [
        "image",
        "services",
        "stages",
        "variables",
        "before_script",
        "after_script",
        "cache",
        "default",
        "include",
        "workflow",
    ]
    .iter()
    .copied()
    .collect();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        let line_indent = line.len() - line.trim_start().len();

        if line_indent == 0 && trimmed.contains(':') && !trimmed.starts_with('-') {
            if let Some(job_name) = current_job.take() {
                if !top_level_keys.contains(job_name.as_str()) {
                    gates.push(ParsedCIGate {
                        job_name,
                        trigger: current_trigger.take(),
                        steps: if current_steps.is_empty() {
                            None
                        } else {
                            Some(current_steps.join("; "))
                        },
                        workflow_name: None,
                        environment: None,
                        artifacts: None,
                        release_gates: None,
                    });
                }
                current_steps.clear();
            }
            let key = trimmed.split(':').next().unwrap().trim();
            current_job = Some(key.to_string());
            if top_level_keys.contains(key) {
                current_job = None;
            }
        }

        if let Some(ref _job) = current_job {
            if trimmed.starts_with("- ") {
                let item = trimmed
                    .strip_prefix("- ")
                    .unwrap()
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'');
                if !item.is_empty() && item.len() < 200 {
                    current_steps.push(item.to_string());
                }
            }
            if (trimmed.starts_with("only:") || trimmed.starts_with("except:"))
                && let Some(trigger_type) = trimmed.strip_suffix(':')
            {
                current_trigger = Some(trigger_type.to_string());
            }
        }
    }

    if let Some(job_name) = current_job.take()
        && !top_level_keys.contains(job_name.as_str())
    {
        gates.push(ParsedCIGate {
            job_name,
            trigger: current_trigger,
            steps: if current_steps.is_empty() {
                None
            } else {
                Some(current_steps.join("; "))
            },
            workflow_name: None,
            environment: None,
            artifacts: None,
            release_gates: None,
        });
    }

    if !stages.is_empty() {
        let trigger_str = stages.join(", ");
        for gate in &mut gates {
            if gate.trigger.is_none() {
                gate.trigger = Some(format!("stages: {}", trigger_str));
            }
        }
    }
    gates
}
