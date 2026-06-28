use super::ParsedCIGate;

pub fn parse_circleci(content: &str) -> Vec<ParsedCIGate> {
    let mut gates = Vec::new();
    let mut in_jobs = false;
    let mut current_job: Option<String> = None;
    let mut current_steps: Vec<String> = Vec::new();
    let mut jobs_indent = 0;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        let line_indent = line.len() - line.trim_start().len();

        if trimmed == "jobs:" || trimmed.starts_with("jobs:") {
            in_jobs = true;
            jobs_indent = line_indent;
            continue;
        }

        if in_jobs {
            if !trimmed.is_empty() && line_indent <= jobs_indent {
                in_jobs = false;
                if let Some(job_name) = current_job.take() {
                    gates.push(ParsedCIGate {
                        job_name,
                        trigger: Some("push".to_string()),
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
                    current_steps.clear();
                }
                continue;
            }

            if line_indent == jobs_indent + 2 && trimmed.ends_with(':') && !trimmed.starts_with('-')
            {
                if let Some(job_name) = current_job.take() {
                    gates.push(ParsedCIGate {
                        job_name,
                        trigger: Some("push".to_string()),
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
                    current_steps.clear();
                }
                current_job = Some(trimmed.strip_suffix(':').unwrap().to_string());
                continue;
            }

            if current_job.is_some()
                && (trimmed.starts_with("- run:") || trimmed.starts_with("- name:"))
            {
                let cmd = trimmed
                    .split_once(':')
                    .map(|x| x.1)
                    .unwrap_or("")
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'');
                if !cmd.is_empty() {
                    current_steps.push(cmd.to_string());
                }
            }
        }
    }

    if let Some(job_name) = current_job.take() {
        gates.push(ParsedCIGate {
            job_name,
            trigger: Some("push".to_string()),
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
    gates
}
