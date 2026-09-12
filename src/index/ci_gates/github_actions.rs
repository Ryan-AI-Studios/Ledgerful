use super::ParsedCIGate;
use std::collections::BTreeSet;

const SKIP_ON_KEYS: &[&str] = &[
    "branches",
    "branches-ignore",
    "tags",
    "tags-ignore",
    "paths",
    "paths-ignore",
    "types",
    "cron",
    "timezone",
    "inputs",
    "secrets",
    "outputs",
];

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
    let mut trigger_set = BTreeSet::new();
    let mut workflow_name: Option<String> = None;
    let mut in_jobs_section = false;

    let mut in_on_section = false;
    let mut on_indent = 0;

    for line in content.lines() {
        let line_indent = line.len() - line.trim_start().len();
        let stripped = strip_unquoted_comment(line.trim());
        if stripped.is_empty() {
            continue;
        }

        if (stripped == "jobs:" || stripped.starts_with("jobs:")) && line_indent == 0 {
            in_jobs_section = true;
        }

        if stripped.starts_with("name:") && !in_jobs_section && line_indent == 0 {
            workflow_name = Some(unquote(stripped.strip_prefix("name:").unwrap_or(stripped)));
        }

        if !in_jobs_section
            && line_indent == 0
            && (stripped == "on:" || stripped.starts_with("on:"))
        {
            in_on_section = true;
            on_indent = line_indent;
            if stripped != "on:" {
                let rest = stripped.strip_prefix("on:").unwrap_or("").trim();
                if !rest.contains('{') {
                    for name in parse_name_list(rest) {
                        trigger_set.insert(name);
                    }
                }
            }
            continue;
        }

        if in_on_section {
            if !stripped.is_empty() && line_indent <= on_indent && !stripped.starts_with('-') {
                in_on_section = false;
            } else if line_indent == on_indent + 2 {
                if stripped.starts_with('-') {
                    let item = stripped.strip_prefix('-').unwrap_or(stripped).trim();
                    for name in parse_name_list(item) {
                        if !SKIP_ON_KEYS.contains(&name.as_str()) {
                            trigger_set.insert(name);
                        }
                    }
                } else if let Some(key) = mapping_key(stripped)
                    && !SKIP_ON_KEYS.contains(&key)
                    && !key.contains(' ')
                {
                    trigger_set.insert(key.to_string());
                }
            }
        }
    }

    let trigger_str = if trigger_set.is_empty() {
        None
    } else {
        Some(trigger_set.into_iter().collect::<Vec<_>>().join(", "))
    };

    let fallback_name = filename.map(workflow_name_from_filename);
    let workflow_name = workflow_name.or(fallback_name);

    let mut draft = JobDraft::default();
    let mut in_jobs_section = false;
    let mut jobs_indent = 0;

    for line in content.lines() {
        let line_indent = line.len() - line.trim_start().len();
        let stripped = strip_unquoted_comment(line.trim());
        if stripped.is_empty() {
            continue;
        }

        if stripped == "jobs:" || stripped.starts_with("jobs:") {
            draft.flush(&mut gates, trigger_str.clone(), workflow_name.clone());
            in_jobs_section = true;
            jobs_indent = line_indent;
            continue;
        }

        if in_jobs_section {
            if !stripped.is_empty() && line_indent <= jobs_indent {
                draft.flush(&mut gates, trigger_str.clone(), workflow_name.clone());
                in_jobs_section = false;
                continue;
            }

            let job_prop_indent = jobs_indent + 4;
            if draft.ingest_continuations(line_indent, job_prop_indent, stripped) {
                continue;
            }

            if line_indent == jobs_indent + 2
                && stripped.ends_with(':')
                && !stripped.starts_with('-')
            {
                draft.flush(&mut gates, trigger_str.clone(), workflow_name.clone());
                draft.name = Some(
                    stripped
                        .strip_suffix(':')
                        .unwrap_or(stripped)
                        .trim()
                        .to_string(),
                );
                continue;
            }

            if draft.name.is_some() && line_indent == job_prop_indent && !draft.in_steps {
                if stripped.starts_with("environment:") {
                    let env = unquote(stripped.strip_prefix("environment:").unwrap_or(""));
                    if !env.is_empty() {
                        draft.env = Some(env);
                    }
                }

                if stripped == "if:" || stripped.starts_with("if:") {
                    let rest = stripped.strip_prefix("if:").unwrap_or("").trim();
                    if is_block_scalar(rest) {
                        draft.if_folding = true;
                        draft.if_fold_lines.clear();
                    } else if !rest.is_empty() {
                        draft.job_if = Some(unquote(rest));
                    }
                    continue;
                }

                if stripped == "needs:" || stripped.starts_with("needs:") {
                    let rest = stripped.strip_prefix("needs:").unwrap_or("").trim();
                    if rest.is_empty() {
                        draft.needs_block = true;
                        draft.needs_items.clear();
                    } else {
                        draft.needs = join_names(parse_name_list(rest));
                    }
                    continue;
                }

                if stripped.starts_with("uses:") {
                    let rest = unquote(stripped.strip_prefix("uses:").unwrap_or(""));
                    if !rest.is_empty() {
                        draft.uses = Some(rest);
                    }
                    continue;
                }

                if stripped == "steps:" || stripped.starts_with("steps:") {
                    draft.in_steps = true;
                    continue;
                }
            }

            if draft.name.is_some() && draft.in_steps && line_indent > 0 {
                if stripped.starts_with("- name:") {
                    let name = unquote(stripped.strip_prefix("- name:").unwrap_or(""));
                    if !name.is_empty() {
                        draft.steps.push(name);
                    }
                } else if stripped.starts_with("- run:") {
                    let cmd = unquote(stripped.strip_prefix("- run:").unwrap_or(""));
                    if !cmd.is_empty() {
                        draft.steps.push(cmd);
                    }
                } else if stripped.contains("uses: actions/upload-artifact") {
                    draft.artifacts.push("upload-artifact".to_string());
                } else if stripped.contains("uses: softprops/action-gh-release") {
                    draft.releases.push("gh-release".to_string());
                }
            }
        }
    }

    draft.flush(&mut gates, trigger_str, workflow_name);
    gates
}

#[derive(Default)]
struct JobDraft {
    name: Option<String>,
    steps: Vec<String>,
    env: Option<String>,
    artifacts: Vec<String>,
    releases: Vec<String>,
    job_if: Option<String>,
    needs: Option<String>,
    uses: Option<String>,
    if_folding: bool,
    if_fold_lines: Vec<String>,
    needs_block: bool,
    needs_items: Vec<String>,
    in_steps: bool,
}

impl JobDraft {
    fn ingest_continuations(
        &mut self,
        line_indent: usize,
        job_prop_indent: usize,
        stripped: &str,
    ) -> bool {
        if self.if_folding {
            if line_indent > job_prop_indent {
                self.if_fold_lines.push(stripped.to_string());
                return true;
            }
            self.finish_if_fold();
        }
        if self.needs_block {
            if line_indent > job_prop_indent && stripped.starts_with('-') {
                let item = stripped.strip_prefix('-').unwrap_or(stripped).trim();
                self.needs_items.extend(parse_name_list(item));
                return true;
            }
            self.finish_needs_block();
        }
        false
    }

    fn finish_if_fold(&mut self) {
        if !self.if_folding {
            return;
        }
        let joined = self
            .if_fold_lines
            .iter()
            .flat_map(|l| l.split_whitespace())
            .collect::<Vec<_>>()
            .join(" ");
        if !joined.is_empty() {
            self.job_if = Some(joined);
        }
        self.if_fold_lines.clear();
        self.if_folding = false;
    }

    fn finish_needs_block(&mut self) {
        if !self.needs_block {
            return;
        }
        self.needs = join_names(std::mem::take(&mut self.needs_items));
        self.needs_block = false;
    }

    fn flush(
        &mut self,
        gates: &mut Vec<ParsedCIGate>,
        trigger: Option<String>,
        workflow_name: Option<String>,
    ) {
        self.finish_if_fold();
        self.finish_needs_block();
        let Some(job_name) = self.name.take() else {
            return;
        };
        gates.push(ParsedCIGate {
            job_name,
            trigger,
            steps: if self.steps.is_empty() {
                None
            } else {
                Some(self.steps.join("; "))
            },
            workflow_name,
            environment: self.env.take(),
            artifacts: if self.artifacts.is_empty() {
                None
            } else {
                Some(std::mem::take(&mut self.artifacts))
            },
            release_gates: if self.releases.is_empty() {
                None
            } else {
                Some(std::mem::take(&mut self.releases))
            },
            job_if: self.job_if.take(),
            needs: self.needs.take(),
            uses: self.uses.take(),
        });
        self.steps.clear();
        self.in_steps = false;
    }
}

fn is_block_scalar(rest: &str) -> bool {
    rest.starts_with('>') || rest.starts_with('|')
}

fn strip_unquoted_comment(s: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    for (i, c) in s.char_indices() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double => return s[..i].trim_end(),
            _ => {}
        }
    }
    s
}

fn mapping_key(stripped: &str) -> Option<&str> {
    let key = stripped.strip_suffix(':')?;
    if key.is_empty() || key.starts_with('#') || key.contains(' ') {
        None
    } else {
        Some(key)
    }
}

fn parse_name_list(raw: &str) -> Vec<String> {
    let s = strip_unquoted_comment(raw).trim();
    if s.is_empty() {
        return Vec::new();
    }
    let inner = if s.starts_with('[') && s.ends_with(']') {
        &s[1..s.len() - 1]
    } else {
        s
    };
    let mut out: Vec<String> = inner
        .split(',')
        .map(unquote)
        .filter(|p| !p.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn join_names(mut names: Vec<String>) -> Option<String> {
    names.sort();
    names.dedup();
    if names.is_empty() {
        None
    } else {
        Some(names.join(", "))
    }
}

fn unquote(s: &str) -> String {
    let t = s.trim();
    let bytes = t.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}
