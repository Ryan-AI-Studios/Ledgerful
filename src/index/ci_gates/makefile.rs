use super::ParsedCIGate;

pub fn parse_makefile(content: &str) -> Vec<ParsedCIGate> {
    let mut gates = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('.') {
            continue;
        }
        if line.starts_with('\t') || line.starts_with(' ') {
            continue;
        }

        if let Some(colon_pos) = trimmed.find(':') {
            let target_part = trimmed[..colon_pos].trim();
            if target_part.is_empty()
                || target_part.contains('=')
                || target_part.contains('%')
                || target_part.starts_with('.')
            {
                continue;
            }

            for target in target_part.split_whitespace() {
                if target.contains('%') || target.contains('(') || target.contains(')') {
                    continue;
                }
                let steps_str = extract_makefile_steps(content, target);
                gates.push(ParsedCIGate {
                    job_name: target.to_string(),
                    trigger: Some("manual".to_string()),
                    steps: if steps_str.is_empty() {
                        None
                    } else {
                        Some(steps_str)
                    },
                    workflow_name: None,
                    environment: None,
                    artifacts: None,
                    release_gates: None,
                });
            }
        }
    }
    gates
}

pub fn extract_makefile_steps(content: &str, target: &str) -> String {
    let mut steps = Vec::new();
    let mut in_target = false;
    for line in content.lines() {
        if !in_target {
            if let Some(colon_pos) = line.find(':')
                && line[..colon_pos].split_whitespace().any(|t| t == target)
            {
                in_target = true;
            }
        } else {
            if line.starts_with('\t') || (line.starts_with("    ") && !line.trim().is_empty()) {
                let cmd = line.trim();
                if !cmd.is_empty() && !cmd.starts_with('#') {
                    if cmd.len() > 200 {
                        steps.push(format!("{}...", &cmd[..197]));
                    } else {
                        steps.push(cmd.to_string());
                    }
                }
            } else if !line.is_empty() && !line.starts_with(' ') && !line.starts_with('\t') {
                break;
            }
        }
    }
    if steps.len() > 20 {
        steps.truncate(20);
    }
    steps.join("; ")
}
