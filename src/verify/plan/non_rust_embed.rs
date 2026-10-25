//! Bounded lexical scan for Python, Go, and JS/TS embedded reads (0570).
//!
//! `EmbeddedScan::scan` stays the Rust walk. This module uses the same caps
//! and `pinned_walk`, and it does not select a test runner.

use super::embedded::{
    EmbeddedDecision, MAX_FILE_BYTES, MAX_FILES_SCANNED, MAX_OVERSIZED_FILES,
    MAX_OVERSIZED_INSPECT_BYTES, MAX_STREAMED_BYTES, MAX_TOTAL_BYTES, MAX_WALK_ENTRIES,
};
use std::collections::BTreeSet;
use std::path::Path;

const SCAN_EXTS: &[&str] = &["cjs", "go", "js", "jsx", "mjs", "py", "ts", "tsx"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NonRustPresence {
    Present,
    Absent,
    Bounded,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    Python,
    Go,
    Script,
}

pub(crate) fn repo_has_non_rust_sources(root: &Path) -> NonRustPresence {
    if !root.is_dir() {
        return NonRustPresence::Absent;
    }
    for (seen, entry) in super::embedded::pinned_walk(root).build().enumerate() {
        if seen >= MAX_WALK_ENTRIES {
            return NonRustPresence::Bounded;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) if seen == 0 => return NonRustPresence::Absent,
            Err(_) => continue,
        };
        if entry.file_type().is_some_and(|kind| kind.is_file()) && is_scan_ext(entry.path()) {
            return NonRustPresence::Present;
        }
    }
    NonRustPresence::Absent
}

pub(crate) fn decide_non_rust(
    repo_root: &Path,
    candidates: &[String],
    is_cheap: impl Fn(&str) -> bool,
) -> EmbeddedDecision {
    let mut kept: Vec<String> = candidates
        .iter()
        .filter(|path| !is_mjs_or_cjs(path))
        .filter_map(|path| super::embedded::normalize_repo_path(path))
        .collect();
    kept.sort();
    kept.dedup();
    if kept.is_empty() {
        return EmbeddedDecision::default();
    }
    match walk_sources(repo_root) {
        Walk::Bound(bound) => apply_bound(&kept, &bound, &is_cheap),
        Walk::Files { files, errors } => {
            if !errors.is_empty() && kept.iter().any(|path| !is_cheap(path)) {
                return read_error_decision(&errors);
            }
            let mut facts = Vec::new();
            for rel in files {
                let full = repo_root.join(&rel);
                let meta = match std::fs::metadata(&full) {
                    Ok(meta) => meta,
                    Err(_) => {
                        if !is_cheap_only(&kept, &is_cheap) {
                            let mut errors = errors.clone();
                            errors.insert(format!("{rel}: unreadable"));
                            return read_error_decision(&errors);
                        }
                        continue;
                    }
                };
                if meta.len() > MAX_FILE_BYTES {
                    let mut budget = StreamBudget::default();
                    let tokens = tokens_for_ext(&rel);
                    match oversized_has_token(&full, &mut budget, tokens) {
                        Ok(Some(false)) => continue,
                        Ok(_) => {
                            return apply_bound(
                                &kept,
                                &format!(
                                    "non-Rust embedded scan bound exceeded: {rel}: oversized (file ceiling {MAX_OVERSIZED_INSPECT_BYTES}; max {MAX_OVERSIZED_FILES} oversized files / {MAX_STREAMED_BYTES} streamed bytes)"
                                ),
                                &is_cheap,
                            );
                        }
                        Err(_) => {
                            if !is_cheap_only(&kept, &is_cheap) {
                                let mut errors = errors.clone();
                                errors.insert(format!("{rel}: unreadable"));
                                return read_error_decision(&errors);
                            }
                            continue;
                        }
                    }
                }
                let bytes = match std::fs::read(&full) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        if !is_cheap_only(&kept, &is_cheap) {
                            let mut errors = errors.clone();
                            errors.insert(format!("{rel}: unreadable"));
                            return read_error_decision(&errors);
                        }
                        continue;
                    }
                };
                let text = match String::from_utf8(bytes) {
                    Ok(text) => text,
                    Err(_) => {
                        if !is_cheap_only(&kept, &is_cheap) {
                            let mut errors = errors.clone();
                            errors.insert(format!("{rel}: nonUtf8"));
                            return read_error_decision(&errors);
                        }
                        continue;
                    }
                };
                let Some(lang) = lang_of(&rel) else {
                    continue;
                };
                facts.push(analyze_file(&rel, &text, lang, repo_root, &kept));
            }
            assemble(&kept, &facts)
        }
    }
}

fn is_cheap_only(candidates: &[String], is_cheap: &impl Fn(&str) -> bool) -> bool {
    candidates.iter().all(|path| is_cheap(path))
}

fn read_error_decision(errors: &BTreeSet<String>) -> EmbeddedDecision {
    let mut decision = EmbeddedDecision::default();
    let first = errors
        .iter()
        .next()
        .map(String::as_str)
        .unwrap_or("unknown: unreadable");
    decision.refuse = Some(format!(
        "non-Rust embedded scan could not read or lex {} file(s) (first: {first})",
        errors.len()
    ));
    decision
}

fn apply_bound(
    candidates: &[String],
    bound: &str,
    is_cheap: &impl Fn(&str) -> bool,
) -> EmbeddedDecision {
    let mut decision = EmbeddedDecision::default();
    let mut refuse = Vec::new();
    let mut saw_cheap = false;
    for path in candidates {
        if is_cheap(path) {
            decision.no_dependency.insert(path.clone());
            saw_cheap = true;
        } else {
            refuse.push(format!("{path}: {bound}"));
        }
    }
    if saw_cheap {
        decision
            .annotations
            .push(format!("embedded lookup bounded: not verified ({bound})"));
    }
    refuse.sort();
    refuse.dedup();
    if !refuse.is_empty() {
        decision.refuse = Some(refuse.join("; "));
    }
    decision
}

enum Walk {
    Bound(String),
    Files {
        files: Vec<String>,
        errors: BTreeSet<String>,
    },
}

fn walk_sources(root: &Path) -> Walk {
    if !root.is_dir() {
        return Walk::Files {
            files: Vec::new(),
            errors: BTreeSet::new(),
        };
    }
    let mut files = Vec::new();
    let mut errors = BTreeSet::new();
    let mut total_bytes = 0u64;
    for (seen, entry) in super::embedded::pinned_walk(root).build().enumerate() {
        if seen >= MAX_WALK_ENTRIES {
            return Walk::Bound(format!(
                "non-Rust embedded scan bound exceeded: {MAX_WALK_ENTRIES} walk entries"
            ));
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                errors.insert("<walk>: unreadable".to_string());
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) || !is_scan_ext(entry.path()) {
            continue;
        }
        let Ok(rel_path) = entry.path().strip_prefix(root) else {
            continue;
        };
        let rel = rel_path.to_string_lossy().replace('\\', "/");
        files.push(rel);
        if files.len() > MAX_FILES_SCANNED {
            return Walk::Bound(format!(
                "non-Rust embedded scan bound exceeded: {} source files (max {MAX_FILES_SCANNED})",
                files.len()
            ));
        }
        if let Ok(meta) = entry.metadata()
            && meta.len() <= MAX_FILE_BYTES
        {
            total_bytes = total_bytes.saturating_add(meta.len());
            if total_bytes > MAX_TOTAL_BYTES {
                return Walk::Bound(format!(
                    "non-Rust embedded scan bound exceeded: {total_bytes} lexed bytes (max {MAX_TOTAL_BYTES})"
                ));
            }
        }
    }
    files.sort();
    Walk::Files { files, errors }
}

#[derive(Default)]
struct StreamBudget {
    files: usize,
    bytes: u64,
}

fn tokens_for_ext(rel: &str) -> &'static [&'static str] {
    match lang_of(rel) {
        Some(Lang::Python) => &["open", "importlib.resources"],
        Some(Lang::Go) => &["go:embed", "ReadFile", "Open"],
        _ => &["readFileSync", "import", "require"],
    }
}

fn oversized_has_token(
    path: &Path,
    budget: &mut StreamBudget,
    tokens: &[&str],
) -> std::io::Result<Option<bool>> {
    budget.files += 1;
    if budget.files > MAX_OVERSIZED_FILES || budget.bytes >= MAX_STREAMED_BYTES {
        return Ok(None);
    }
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut chunk = [0u8; 64 * 1024];
    let mut overlap = Vec::new();
    let mut inspected = 0u64;
    let keep = tokens
        .iter()
        .map(|token| token.len())
        .max()
        .unwrap_or(1)
        .saturating_sub(1);
    loop {
        let remaining =
            (MAX_OVERSIZED_INSPECT_BYTES - inspected).min(MAX_STREAMED_BYTES - budget.bytes);
        if remaining == 0 {
            let mut probe = [0u8; 1];
            return Ok(if file.read(&mut probe)? == 0 {
                Some(false)
            } else {
                None
            });
        }
        let read_len = chunk.len().min(remaining as usize);
        let n = file.read(&mut chunk[..read_len])?;
        if n == 0 {
            return Ok(Some(false));
        }
        inspected += n as u64;
        budget.bytes += n as u64;
        if inspected > MAX_OVERSIZED_INSPECT_BYTES {
            return Ok(None);
        }
        overlap.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&overlap);
        if tokens.iter().any(|token| text.contains(token)) {
            return Ok(Some(true));
        }
        if overlap.len() > keep {
            overlap.drain(..overlap.len() - keep);
        }
    }
}

struct HelperDef {
    name: String,
    form: &'static str,
}

struct HelperCall {
    name: String,
    literals: Vec<String>,
}

struct DirectHit {
    candidate: String,
    form: &'static str,
}

struct FileFacts {
    rel: String,
    dir: String,
    lang: Lang,
    helpers: Vec<HelperDef>,
    calls: Vec<HelperCall>,
    direct: Vec<DirectHit>,
}

fn assemble(candidates: &[String], files: &[FileFacts]) -> EmbeddedDecision {
    let mut reasons = BTreeSet::new();
    for file in files {
        for hit in &file.direct {
            reasons.insert(format!(
                "{}: non-Rust embedded read ({}: {})",
                hit.candidate, file.rel, hit.form
            ));
        }
    }
    for call_file in files {
        for call in &call_file.calls {
            for def_file in files {
                for def in &def_file.helpers {
                    if !helpers_match(call_file, &call.name, def_file, &def.name) {
                        continue;
                    }
                    for literal in &call.literals {
                        for candidate in candidates {
                            if literal_matches(&call_file.rel, literal, candidate) {
                                reasons.insert(format!(
                                    "{candidate}: non-Rust embedded read ({}: {})",
                                    call_file.rel, def.form
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    let mut decision = EmbeddedDecision::default();
    for candidate in candidates {
        let prefix = format!("{candidate}: ");
        if !reasons.iter().any(|reason| reason.starts_with(&prefix)) {
            decision.no_dependency.insert(candidate.clone());
        }
    }
    if !reasons.is_empty() {
        decision.refuse = Some(reasons.into_iter().collect::<Vec<_>>().join("; "));
    }
    decision
}

fn helpers_match(
    call_file: &FileFacts,
    call_name: &str,
    def_file: &FileFacts,
    def_name: &str,
) -> bool {
    if call_name != def_name {
        return false;
    }
    match (call_file.lang, def_file.lang) {
        (Lang::Go, Lang::Go) => call_file.dir == def_file.dir,
        (Lang::Go, _) | (_, Lang::Go) => false,
        _ => true,
    }
}

fn analyze_file(rel: &str, src: &str, lang: Lang, root: &Path, candidates: &[String]) -> FileFacts {
    let has_importlib = lang == Lang::Python && code_contains(src, lang, "importlib.resources");
    let mut facts = FileFacts {
        rel: rel.to_string(),
        dir: parent_dir(rel).to_string(),
        lang,
        helpers: Vec::new(),
        calls: Vec::new(),
        direct: Vec::new(),
    };
    let mut lex = Lex {
        src,
        lang,
        rel,
        candidates,
        has_importlib,
        indent: 0,
        py: Vec::new(),
        braces: Vec::new(),
        pending_fn: None,
        pending_const: None,
        expr_fn: None,
    };
    let mut brace_depth = 0i32;
    let bytes = src.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if lang == Lang::Python && is_line_start(src, i) {
            match line_kind(src, i) {
                LineKind::Ignorable => {
                    i = skip_line(src, i);
                    continue;
                }
                LineKind::Code { indent, at } => {
                    while lex.py.last().is_some_and(|item| indent <= item.indent) {
                        lex.py.pop();
                    }
                    lex.indent = indent;
                    i = at;
                }
            }
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        if bytes[i].is_ascii_whitespace() {
            if bytes[i] == b'\n' {
                lex.expr_fn = None;
            }
            i += 1;
        } else if lang == Lang::Python && bytes[i] == b'#' {
            i = skip_line_comment(src, i);
        } else if lang != Lang::Python && bytes[i] == b'/' && src[i..].starts_with("//") {
            if lang == Lang::Go && is_go_embed_line(src, i) {
                let rest = &src[i + "//go:embed".len()..];
                let line_end = rest.find('\n').unwrap_or(rest.len());
                for (all, pattern) in embed_patterns(&rest[..line_end]) {
                    for candidate in candidates {
                        if embed_matches(root, rel, all, &pattern, candidate) {
                            facts.direct.push(DirectHit {
                                candidate: candidate.clone(),
                                form: "go:embed",
                            });
                        }
                    }
                }
                i = skip_line_comment(src, i);
            } else {
                i = skip_line_comment(src, i);
            }
        } else if lang != Lang::Python && bytes[i] == b'/' && src[i..].starts_with("/*") {
            i = skip_block_comment(src, i);
        } else if let Some(token) = try_string(src, i, lang) {
            i = token.end;
        } else if let Some((end, ident)) = read_ident(src, i) {
            i = dispatch_ident(&mut lex, &mut facts, ident, end);
        } else if bytes[i] == b'{' && lang != Lang::Python {
            brace_depth += 1;
            if let Some(name) = lex.pending_fn.take() {
                lex.braces.push(BraceFn {
                    name,
                    depth: brace_depth,
                });
            }
            i += 1;
        } else if bytes[i] == b'}' && lang != Lang::Python {
            if lex
                .braces
                .last()
                .is_some_and(|item| item.depth == brace_depth)
            {
                lex.braces.pop();
            }
            brace_depth -= 1;
            i += 1;
        } else if bytes[i] == b';' {
            lex.expr_fn = None;
            i += 1;
        } else {
            i += utf8_len(bytes[i]);
        }
        if i <= start {
            i = start + utf8_len(bytes[start]);
        }
    }
    facts
}

struct Lex<'a> {
    src: &'a str,
    lang: Lang,
    rel: &'a str,
    candidates: &'a [String],
    has_importlib: bool,
    indent: usize,
    py: Vec<PyFn>,
    braces: Vec<BraceFn>,
    pending_fn: Option<String>,
    pending_const: Option<String>,
    expr_fn: Option<String>,
}

struct PyFn {
    indent: usize,
    name: String,
}

struct BraceFn {
    name: String,
    depth: i32,
}

fn dispatch_ident(
    lex: &mut Lex<'_>,
    facts: &mut FileFacts,
    ident: &str,
    ident_end: usize,
) -> usize {
    let src = lex.src;
    let lang = lex.lang;
    if lang == Lang::Python && ident == "def" {
        let after = skip_ws_and_comments(src, ident_end, lang);
        if let Some((end, name)) = read_ident(src, after) {
            lex.py.push(PyFn {
                indent: lex.indent,
                name: name.to_string(),
            });
            return end;
        }
        return ident_end;
    }
    if lang == Lang::Go && ident == "func" {
        return consume_go_func(src, ident_end, &mut lex.pending_fn);
    }
    if lang == Lang::Script && ident == "function" {
        return consume_js_function(src, ident_end, &mut lex.pending_fn, &mut lex.pending_const);
    }
    if lang == Lang::Script && matches!(ident, "const" | "let" | "var") {
        return consume_js_binding(
            src,
            ident_end,
            &mut lex.pending_fn,
            &mut lex.pending_const,
            &mut lex.expr_fn,
        );
    }
    if lang == Lang::Script && (ident == "import" || ident == "export") {
        let after = skip_ws_and_comments(src, ident_end, lang);
        if !(ident == "import" && src[after..].starts_with('(')) {
            if let Some((end, spec)) = import_specifier(src, ident_end, lang) {
                record_import(lex.rel, &spec, lex.candidates, facts);
                return end;
            }
            return ident_end;
        }
    }
    if let Some((end, parts, args)) = read_call(src, ident_end - ident.len(), lang) {
        let enclosing = enclosing_name(&lex.py, &lex.braces, &lex.expr_fn);
        record_call(
            facts,
            lex.has_importlib,
            &parts,
            &args,
            enclosing,
            lex.candidates,
        );
        return end;
    }
    ident_end
}

fn enclosing_name<'a>(
    py_stack: &'a [PyFn],
    brace_stack: &'a [BraceFn],
    expr_fn: &'a Option<String>,
) -> Option<&'a str> {
    if let Some(name) = expr_fn {
        return Some(name);
    }
    if let Some(item) = brace_stack.last() {
        return Some(&item.name);
    }
    py_stack.last().map(|item| item.name.as_str())
}

fn consume_go_func(src: &str, ident_end: usize, pending_fn: &mut Option<String>) -> usize {
    let mut j = skip_ws_and_comments(src, ident_end, Lang::Go);
    if src[j..].starts_with('(') {
        j = skip_group(src, j, Lang::Go);
        j = skip_ws_and_comments(src, j, Lang::Go);
    }
    if let Some((end, name)) = read_ident(src, j) {
        *pending_fn = Some(name.to_string());
        return end;
    }
    ident_end
}

fn consume_js_function(
    src: &str,
    ident_end: usize,
    pending_fn: &mut Option<String>,
    pending_const: &mut Option<String>,
) -> usize {
    let after = skip_ws_and_comments(src, ident_end, Lang::Script);
    if let Some((end, name)) = read_ident(src, after) {
        *pending_fn = Some(name.to_string());
        pending_const.take();
        return end;
    }
    if let Some(name) = pending_const.take() {
        *pending_fn = Some(name);
    }
    after
}

fn consume_js_binding(
    src: &str,
    ident_end: usize,
    pending_fn: &mut Option<String>,
    pending_const: &mut Option<String>,
    expr_fn: &mut Option<String>,
) -> usize {
    let after_kw = skip_ws_and_comments(src, ident_end, Lang::Script);
    let Some((name_end, name)) = read_ident(src, after_kw) else {
        return ident_end;
    };
    let after_name = skip_ws_and_comments(src, name_end, Lang::Script);
    if !src[after_name..].starts_with('=') {
        return name_end;
    }
    let after_eq = skip_ws_and_comments(src, after_name + 1, Lang::Script);
    if src[after_eq..].starts_with("function") {
        *pending_const = Some(name.to_string());
        return after_eq;
    }
    if src[after_eq..].starts_with('(') {
        let after_params = skip_group(src, after_eq, Lang::Script);
        let after_params = skip_ws_and_comments(src, after_params, Lang::Script);
        if src[after_params..].starts_with("=>") {
            let body = skip_ws_and_comments(src, after_params + 2, Lang::Script);
            if src[body..].starts_with('{') {
                *pending_fn = Some(name.to_string());
                return body;
            }
            *expr_fn = Some(name.to_string());
            return body;
        }
    }
    name_end
}

fn record_import(rel: &str, spec: &str, candidates: &[String], facts: &mut FileFacts) {
    for candidate in candidates {
        if import_matches(rel, spec, candidate) {
            facts.direct.push(DirectHit {
                candidate: candidate.clone(),
                form: "import",
            });
        }
    }
}

fn record_call(
    facts: &mut FileFacts,
    has_importlib: bool,
    parts: &[String],
    args: &[Arg],
    enclosing: Option<&str>,
    candidates: &[String],
) {
    let rel = facts.rel.clone();
    let lang = facts.lang;
    if let Some(spec) = js_module_spec(lang, parts, args) {
        record_import(&rel, &spec, candidates, facts);
    }
    if let Some(modeled) = modeled_read(lang, has_importlib, parts, args) {
        if modeled.literal {
            for literal in &modeled.paths {
                for candidate in candidates {
                    if literal_matches(&rel, literal, candidate) {
                        facts.direct.push(DirectHit {
                            candidate: candidate.clone(),
                            form: modeled.form,
                        });
                    }
                }
            }
        } else if let Some(name) = enclosing {
            facts.helpers.push(HelperDef {
                name: name.to_string(),
                form: modeled.form,
            });
        }
        return;
    }
    let literals = positional_literals(args);
    if literals.is_empty() {
        return;
    }
    let Some(name) = parts.last() else {
        return;
    };
    facts.calls.push(HelperCall {
        name: name.clone(),
        literals,
    });
}

struct Modeled {
    form: &'static str,
    paths: Vec<String>,
    literal: bool,
}

fn modeled_read(
    lang: Lang,
    has_importlib: bool,
    parts: &[String],
    args: &[Arg],
) -> Option<Modeled> {
    let name = parts.last()?.as_str();
    match lang {
        Lang::Python if name == "open" => open_modeled(args),
        Lang::Python if has_importlib => importlib_modeled(name, args),
        Lang::Go if name == "ReadFile" && !is_ioutil(parts) => first_pos(args, "os.ReadFile"),
        Lang::Go if name == "Open" => first_pos(args, "os.Open"),
        Lang::Script if name == "readFileSync" => first_pos(args, "readFileSync"),
        _ => None,
    }
}

fn is_ioutil(parts: &[String]) -> bool {
    parts.len() >= 2 && parts[parts.len() - 2] == "ioutil"
}

fn open_modeled(args: &[Arg]) -> Option<Modeled> {
    if let Some(first) = args.iter().find(|arg| arg.keyword.is_none()) {
        if let Some(literal) = &first.literal {
            return Some(Modeled {
                form: "open",
                paths: vec![literal.clone()],
                literal: true,
            });
        }
        return Some(Modeled {
            form: "open",
            paths: Vec::new(),
            literal: false,
        });
    }
    if let Some(file) = args
        .iter()
        .find(|arg| arg.keyword.as_deref() == Some("file"))
    {
        if let Some(literal) = &file.literal {
            return Some(Modeled {
                form: "open",
                paths: vec![literal.clone()],
                literal: true,
            });
        }
        return Some(Modeled {
            form: "open",
            paths: Vec::new(),
            literal: false,
        });
    }
    None
}

fn importlib_modeled(name: &str, args: &[Arg]) -> Option<Modeled> {
    const NAMES: &[&str] = &[
        "read_text",
        "read_binary",
        "open_text",
        "open_binary",
        "joinpath",
        "read_bytes",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    if name == "joinpath" {
        if args.is_empty() {
            return None;
        }
        if args.iter().any(|arg| arg.literal.is_none()) {
            return Some(Modeled {
                form: "importlib.resources",
                paths: Vec::new(),
                literal: false,
            });
        }
        let joined = args
            .iter()
            .filter_map(|arg| arg.literal.clone())
            .collect::<Vec<_>>()
            .join("/");
        return Some(Modeled {
            form: "importlib.resources",
            paths: vec![joined],
            literal: true,
        });
    }
    let mut paths = Vec::new();
    let mut non_literal = false;
    for arg in args {
        if let Some(literal) = &arg.literal {
            paths.push(literal.clone());
        } else if arg.non_literal {
            non_literal = true;
        }
    }
    if non_literal {
        return Some(Modeled {
            form: "importlib.resources",
            paths: Vec::new(),
            literal: false,
        });
    }
    if paths.is_empty() {
        return None;
    }
    Some(Modeled {
        form: "importlib.resources",
        paths,
        literal: true,
    })
}

fn first_pos(args: &[Arg], form: &'static str) -> Option<Modeled> {
    let first = args.iter().find(|arg| arg.keyword.is_none())?;
    if let Some(literal) = &first.literal {
        return Some(Modeled {
            form,
            paths: vec![literal.clone()],
            literal: true,
        });
    }
    Some(Modeled {
        form,
        paths: Vec::new(),
        literal: false,
    })
}

fn js_module_spec(lang: Lang, parts: &[String], args: &[Arg]) -> Option<String> {
    if lang != Lang::Script {
        return None;
    }
    let name = parts.last()?.as_str();
    if name != "require" && name != "import" {
        return None;
    }
    let first = args.iter().find(|arg| arg.keyword.is_none())?;
    first.literal.clone()
}

fn positional_literals(args: &[Arg]) -> Vec<String> {
    args.iter()
        .filter(|arg| arg.keyword.is_none())
        .filter_map(|arg| arg.literal.clone())
        .collect()
}

struct Arg {
    keyword: Option<String>,
    literal: Option<String>,
    non_literal: bool,
}

struct ArgBuild {
    ident: Option<String>,
    keyword: Option<String>,
    literal: Option<String>,
    saw_other: bool,
}

impl ArgBuild {
    fn fresh() -> Self {
        Self {
            ident: None,
            keyword: None,
            literal: None,
            saw_other: false,
        }
    }

    fn see_ident(&mut self, name: &str) {
        if self.literal.is_some() || self.keyword.is_some() || self.saw_other {
            self.saw_other = true;
        } else {
            self.ident = Some(name.to_string());
        }
    }

    fn see_eq(&mut self) {
        if let Some(name) = self.ident.take()
            && self.literal.is_none()
            && !self.saw_other
            && self.keyword.is_none()
        {
            self.keyword = Some(name);
            return;
        }
        self.saw_other = true;
    }

    fn see_string(&mut self, literal: bool, value: String) {
        if !literal || self.saw_other || self.literal.is_some() {
            self.saw_other = true;
            return;
        }
        if self.keyword.is_some() || self.ident.is_none() {
            self.literal = Some(value);
            return;
        }
        self.saw_other = true;
    }

    fn see_other(&mut self) {
        self.saw_other = true;
    }

    fn is_empty(&self) -> bool {
        self.ident.is_none() && self.keyword.is_none() && self.literal.is_none() && !self.saw_other
    }

    fn finish(self) -> Arg {
        let pure = self.literal.is_some() && !self.saw_other;
        let non_literal =
            !pure && (self.saw_other || self.ident.is_some() || self.keyword.is_some());
        Arg {
            keyword: self.keyword,
            literal: if pure { self.literal } else { None },
            non_literal,
        }
    }
}

fn read_call(src: &str, start: usize, lang: Lang) -> Option<(usize, Vec<String>, Vec<Arg>)> {
    let mut parts = Vec::new();
    let mut j = start;
    loop {
        let (end, ident) = read_ident(src, j)?;
        parts.push(ident.to_string());
        j = skip_ws_and_comments(src, end, lang);
        if src[j..].starts_with('.') {
            j = skip_ws_and_comments(src, j + 1, lang);
            continue;
        }
        if src[j..].starts_with('(') {
            let (end, args) = parse_args(src, j, lang)?;
            return Some((end, parts, args));
        }
        return None;
    }
}

fn parse_args(src: &str, open: usize, lang: Lang) -> Option<(usize, Vec<Arg>)> {
    if !src[open..].starts_with('(') {
        return None;
    }
    let bytes = src.as_bytes();
    let mut j = open + 1;
    let mut depth = 1i32;
    let mut args = Vec::new();
    let mut build = ArgBuild::fresh();
    while j < bytes.len() && depth > 0 {
        if bytes[j].is_ascii_whitespace() {
            j += 1;
            continue;
        }
        if lang == Lang::Python && bytes[j] == b'#' {
            j = skip_line_comment(src, j);
            continue;
        }
        if lang != Lang::Python && bytes[j] == b'/' && src[j..].starts_with("//") {
            j = skip_line_comment(src, j);
            continue;
        }
        if lang != Lang::Python && bytes[j] == b'/' && src[j..].starts_with("/*") {
            j = skip_block_comment(src, j);
            continue;
        }
        if let Some(token) = try_string(src, j, lang) {
            if depth == 1 {
                build.see_string(token.literal, token.value);
            }
            j = token.end;
            continue;
        }
        match bytes[j] {
            b'(' | b'[' | b'{' => {
                if depth == 1 {
                    build.see_other();
                }
                depth += 1;
                j += 1;
            }
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    if !build.is_empty() {
                        args.push(build.finish());
                    }
                    j += 1;
                    break;
                }
                j += 1;
            }
            b',' if depth == 1 => {
                if !build.is_empty() {
                    args.push(build.finish());
                }
                build = ArgBuild::fresh();
                j += 1;
            }
            b',' => j += 1,
            b'=' if depth == 1 => {
                build.see_eq();
                j += 1;
            }
            _ => {
                if depth == 1 {
                    if let Some((end, ident)) = read_ident(src, j) {
                        if lang == Lang::Python
                            && is_py_prefix(ident)
                            && let Some(token) = try_string(src, end, lang)
                        {
                            build.see_string(token.literal, token.value);
                            j = token.end;
                            continue;
                        }
                        build.see_ident(ident);
                        j = end;
                    } else {
                        build.see_other();
                        j += utf8_len(bytes[j]);
                    }
                } else if let Some((end, _)) = read_ident(src, j) {
                    j = end;
                } else {
                    j += utf8_len(bytes[j]);
                }
            }
        }
    }
    Some((j, args))
}

struct StrTok {
    end: usize,
    value: String,
    literal: bool,
}

fn try_string(src: &str, i: usize, lang: Lang) -> Option<StrTok> {
    let bytes = src.as_bytes();
    if i >= bytes.len() {
        return None;
    }
    match lang {
        Lang::Python => python_string(src, i),
        Lang::Go => go_string(src, i),
        Lang::Script => script_string(src, i),
    }
}

fn python_string(src: &str, i: usize) -> Option<StrTok> {
    let bytes = src.as_bytes();
    if !matches!(bytes.get(i), Some(b'"' | b'\'')) {
        return None;
    }
    let (prefix_start, raw, formatted) = python_prefix(src, i);
    if prefix_start != i && !is_py_prefix(&src[prefix_start..i]) {
        return None;
    }
    let quote = bytes[i];
    let triple = src[i..].starts_with(&format!(
        "{}{}{}",
        quote as char, quote as char, quote as char
    ));
    let body_start = if triple { i + 3 } else { i + 1 };
    let (body_end, end) = close_python_string(src, body_start, quote, triple)?;
    let raw_body = &src[body_start..body_end];
    let interpolated = formatted && py_interpolates(raw_body, raw);
    let value = if raw {
        raw_body.to_string()
    } else {
        unescape(raw_body, false)
    };
    Some(StrTok {
        end,
        value,
        literal: !interpolated,
    })
}

fn python_prefix(src: &str, quote_at: usize) -> (usize, bool, bool) {
    let bytes = src.as_bytes();
    let mut j = quote_at;
    while j > 0 && bytes[j - 1].is_ascii_alphabetic() {
        j -= 1;
    }
    let prefix = &src[j..quote_at];
    if !is_py_prefix(prefix) {
        return (quote_at, false, false);
    }
    let raw = prefix.bytes().any(|b| b == b'r' || b == b'R');
    let formatted = prefix.bytes().any(|b| b == b'f' || b == b'F');
    (j, raw, formatted)
}

fn is_py_prefix(prefix: &str) -> bool {
    !prefix.is_empty()
        && prefix.len() <= 3
        && prefix
            .bytes()
            .all(|b| matches!(b, b'r' | b'R' | b'b' | b'B' | b'f' | b'F' | b'u' | b'U'))
}

fn close_python_string(src: &str, start: usize, quote: u8, triple: bool) -> Option<(usize, usize)> {
    let bytes = src.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if triple
            && src[i..].starts_with(&format!(
                "{}{}{}",
                quote as char, quote as char, quote as char
            ))
        {
            return Some((i, i + 3));
        }
        if !triple && bytes[i] == quote {
            return Some((i, i + 1));
        }
        i += utf8_len(bytes[i]);
    }
    None
}

fn py_interpolates(raw: &str, is_raw: bool) -> bool {
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !is_raw && bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'{' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                i += 2;
                continue;
            }
            return true;
        }
        i += 1;
    }
    false
}

fn go_string(src: &str, i: usize) -> Option<StrTok> {
    let bytes = src.as_bytes();
    if bytes.get(i) == Some(&b'`') {
        let rest = &src[i + 1..];
        let end_rel = rest.find('`')?;
        return Some(StrTok {
            end: i + 1 + end_rel + 1,
            value: rest[..end_rel].to_string(),
            literal: true,
        });
    }
    if bytes.get(i) != Some(&b'"') {
        return None;
    }
    let (body_end, end) = close_quoted(src, i + 1, b'"')?;
    Some(StrTok {
        end,
        value: unescape(&src[i + 1..body_end], false),
        literal: true,
    })
}

fn script_string(src: &str, i: usize) -> Option<StrTok> {
    let bytes = src.as_bytes();
    let quote = *bytes.get(i)?;
    if !matches!(quote, b'"' | b'\'' | b'`') {
        return None;
    }
    let (body_end, end) = close_quoted(src, i + 1, quote)?;
    let raw = &src[i + 1..body_end];
    let literal = quote != b'`' || !raw.contains("${");
    Some(StrTok {
        end,
        value: unescape(raw, quote == b'`'),
        literal,
    })
}

fn close_quoted(src: &str, start: usize, quote: u8) -> Option<(usize, usize)> {
    let bytes = src.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            return Some((i, i + 1));
        }
        if bytes[i] == b'\n' && quote != b'`' {
            return None;
        }
        i += utf8_len(bytes[i]);
    }
    None
}

fn unescape(raw: &str, template: bool) -> String {
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some('`') if template => out.push('`'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn code_contains(src: &str, lang: Lang, needle: &str) -> bool {
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if lang == Lang::Python && bytes[i] == b'#' {
            i = skip_line_comment(src, i);
            continue;
        }
        if let Some(token) = try_string(src, i, lang) {
            i = token.end;
            continue;
        }
        if src[i..].starts_with(needle) {
            return true;
        }
        i += utf8_len(bytes[i]);
    }
    false
}

fn import_specifier(src: &str, keyword_end: usize, lang: Lang) -> Option<(usize, String)> {
    let bytes = src.as_bytes();
    let mut j = keyword_end;
    let mut depth = 0i32;
    let mut first = true;
    while j < bytes.len() {
        if bytes[j].is_ascii_whitespace() {
            j += 1;
            continue;
        }
        if bytes[j] == b'/' && src[j..].starts_with("//") {
            j = skip_line_comment(src, j);
            continue;
        }
        if bytes[j] == b'/' && src[j..].starts_with("/*") {
            j = skip_block_comment(src, j);
            continue;
        }
        if first {
            first = false;
            if let Some(token) = try_string(src, j, lang) {
                if token.literal {
                    return Some((token.end, token.value));
                }
                return None;
            }
        }
        if let Some((end, ident)) = read_ident(src, j) {
            if ident == "from" && depth == 0 {
                let after = skip_ws_and_comments(src, end, lang);
                if let Some(token) = try_string(src, after, lang)
                    && token.literal
                {
                    return Some((token.end, token.value));
                }
            }
            j = end;
            continue;
        }
        if let Some(token) = try_string(src, j, lang) {
            j = token.end;
            continue;
        }
        match bytes[j] {
            b'(' | b'[' | b'{' => {
                depth += 1;
                j += 1;
            }
            b')' | b']' | b'}' => {
                depth -= 1;
                j += 1;
            }
            b';' | b'\n' if depth == 0 => return None,
            _ => j += utf8_len(bytes[j]),
        }
    }
    None
}

fn embed_patterns(rest: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut rest = rest.trim_start();
    while !rest.is_empty() {
        if (rest.starts_with('"') || rest.starts_with('`'))
            && let Some(token) = go_string(rest, 0)
        {
            push_embed(&mut out, &token.value);
            rest = rest[token.end..].trim_start();
            continue;
        }
        let (tok, next) = match rest.split_once(char::is_whitespace) {
            Some((tok, next)) => (tok, next),
            None => (rest, ""),
        };
        push_embed(&mut out, tok);
        rest = next.trim_start();
    }
    out
}

fn push_embed(out: &mut Vec<(bool, String)>, token: &str) {
    if token.is_empty() {
        return;
    }
    if let Some(pattern) = token.strip_prefix("all:") {
        if !pattern.is_empty() {
            out.push((true, pattern.to_string()));
        }
    } else {
        out.push((false, token.to_string()));
    }
}

fn embed_matches(root: &Path, go_rel: &str, all: bool, pattern: &str, candidate: &str) -> bool {
    if pattern_rejected(pattern) {
        return false;
    }
    let dir = parent_dir(go_rel);
    let Some(joined) = join_norm(dir, pattern) else {
        return false;
    };
    if pattern.contains('*') || pattern.contains('?') {
        let Some(relative) = strip_dir(candidate, dir) else {
            return false;
        };
        return glob_match(pattern, &relative, all);
    }
    let target = root.join(&joined);
    prefix_match(&joined, candidate, all, target.is_dir(), target.exists())
}

fn pattern_rejected(pattern: &str) -> bool {
    if pattern.starts_with('/') || pattern.ends_with('/') {
        return true;
    }
    pattern
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
}

fn prefix_match(joined: &str, candidate: &str, all: bool, is_dir: bool, exists: bool) -> bool {
    if same_path(joined, candidate) {
        return true;
    }
    let prefix = format!("{}/", fold_path(joined));
    let folded = fold_path(candidate);
    let Some(rest) = folded.strip_prefix(&prefix) else {
        return false;
    };
    if rest.is_empty() || !(is_dir || !exists) {
        return false;
    }
    if !all
        && rest
            .split('/')
            .any(|seg| seg.starts_with('.') || seg.starts_with('_'))
    {
        return false;
    }
    true
}

fn strip_dir(candidate: &str, dir: &str) -> Option<String> {
    if dir.is_empty() {
        return Some(candidate.to_string());
    }
    let prefix = format!("{}/", fold_path(dir));
    fold_path(candidate)
        .strip_prefix(&prefix)
        .map(str::to_string)
}

fn glob_match(pattern: &str, relative: &str, all: bool) -> bool {
    let pats: Vec<&str> = pattern.split('/').collect();
    let segs: Vec<&str> = relative.split('/').filter(|seg| !seg.is_empty()).collect();
    if pats.len() != segs.len() {
        return false;
    }
    pats.iter()
        .zip(segs.iter())
        .all(|(pat, seg)| segment_match(pat, seg, all))
}

fn segment_match(pat: &str, seg: &str, all: bool) -> bool {
    if !all
        && (seg.starts_with('.') || seg.starts_with('_'))
        && (pat.contains('*') || pat.contains('?'))
    {
        return false;
    }
    glob_segment(&fold_path(pat), &fold_path(seg))
}

fn glob_segment(pat: &str, text: &str) -> bool {
    fn rec(pat: &[u8], text: &[u8]) -> bool {
        let mut i = 0;
        let mut j = 0;
        while i < pat.len() {
            if pat[i] == b'*' {
                if i + 1 == pat.len() {
                    return true;
                }
                while j <= text.len() {
                    if rec(&pat[i + 1..], &text[j..]) {
                        return true;
                    }
                    if j == text.len() {
                        return false;
                    }
                    j += 1;
                }
                return false;
            }
            if j == text.len() || (pat[i] != b'?' && pat[i] != text[j]) {
                return false;
            }
            i += 1;
            j += 1;
        }
        j == text.len()
    }
    rec(pat.as_bytes(), text.as_bytes())
}

fn import_matches(source_rel: &str, spec: &str, candidate: &str) -> bool {
    let spec = strip_query_hash(spec);
    if !(spec.starts_with("./") || spec.starts_with("../")) {
        return false;
    }
    if final_extension(spec).is_some_and(|ext| ext_is_code(&ext)) {
        return false;
    }
    let Some(resolved) = join_norm(parent_dir(source_rel), spec) else {
        return false;
    };
    if final_extension(spec).is_some() {
        return same_path(&resolved, candidate);
    }
    if same_path(&resolved, candidate) {
        return true;
    }
    let Some((stem, ext)) = split_final_ext(candidate) else {
        return false;
    };
    !ext_is_code(&ext) && same_path(&stem, &resolved)
}

fn literal_matches(source_rel: &str, literal: &str, candidate: &str) -> bool {
    let mut found = Vec::new();
    if let Some(path) = join_norm("", literal) {
        found.push(path);
    }
    if let Some(path) = join_norm(parent_dir(source_rel), literal)
        && !found.iter().any(|item| item == &path)
    {
        found.push(path);
    }
    found.iter().any(|path| same_path(path, candidate))
}

fn join_norm(base_dir: &str, rel: &str) -> Option<String> {
    if rel.starts_with('/') || rel.starts_with('\\') || rel.as_bytes().get(1) == Some(&b':') {
        return None;
    }
    if base_dir.is_empty() {
        super::embedded::normalize_repo_path(rel)
    } else {
        super::embedded::normalize_repo_path(&format!("{base_dir}/{rel}"))
    }
}

fn parent_dir(rel: &str) -> &str {
    rel.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
}

fn fold_path(path: &str) -> String {
    if cfg!(windows) {
        path.to_ascii_lowercase()
    } else {
        path.to_string()
    }
}

fn same_path(left: &str, right: &str) -> bool {
    fold_path(left) == fold_path(right)
}

fn is_scan_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| SCAN_EXTS.iter().any(|item| ext.eq_ignore_ascii_case(item)))
}

fn is_mjs_or_cjs(path: &str) -> bool {
    matches!(final_extension(path).as_deref(), Some("mjs") | Some("cjs"))
}

fn lang_of(rel: &str) -> Option<Lang> {
    match final_extension(rel).as_deref() {
        Some("py") => Some(Lang::Python),
        Some("go") => Some(Lang::Go),
        Some("js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx") => Some(Lang::Script),
        _ => None,
    }
}

fn ext_is_code(ext: &str) -> bool {
    let lower = ext.to_ascii_lowercase();
    SCAN_EXTS.iter().any(|item| *item == lower)
        || super::embedded::is_source_path(Path::new(&format!("f.{lower}")))
}

fn final_extension(path: &str) -> Option<String> {
    split_final_ext(path).map(|(_, ext)| ext)
}

fn split_final_ext(path: &str) -> Option<(String, String)> {
    let slash = path.rfind('/');
    let (dir, name) = match slash {
        Some(index) => (&path[..index], &path[index + 1..]),
        None => ("", path),
    };
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() {
        return None;
    }
    let stem_path = if dir.is_empty() {
        stem.to_string()
    } else {
        format!("{dir}/{stem}")
    };
    Some((stem_path, ext.to_ascii_lowercase()))
}

fn strip_query_hash(spec: &str) -> &str {
    match spec.find(['?', '#']) {
        Some(index) => &spec[..index],
        None => spec,
    }
}

enum LineKind {
    Ignorable,
    Code { indent: usize, at: usize },
}

fn line_kind(src: &str, i: usize) -> LineKind {
    let bytes = src.as_bytes();
    let mut j = i;
    let mut indent = 0usize;
    while j < bytes.len() && matches!(bytes[j], b' ' | b'\t') {
        indent += 1;
        j += 1;
    }
    if j >= bytes.len() || matches!(bytes[j], b'\n' | b'\r' | b'#') {
        LineKind::Ignorable
    } else {
        LineKind::Code { indent, at: j }
    }
}

fn is_line_start(src: &str, i: usize) -> bool {
    i == 0 || src.as_bytes().get(i - 1) == Some(&b'\n')
}

fn is_go_embed_line(src: &str, slash: usize) -> bool {
    if !src[slash..].starts_with("//go:embed") {
        return false;
    }
    let line_start = src[..slash].rfind('\n').map_or(0, |index| index + 1);
    src[line_start..slash].trim().is_empty()
}

fn skip_line(src: &str, i: usize) -> usize {
    let bytes = src.as_bytes();
    let mut j = i;
    while j < bytes.len() && bytes[j] != b'\n' {
        j += 1;
    }
    if j < bytes.len() { j + 1 } else { j }
}

fn skip_line_comment(src: &str, i: usize) -> usize {
    let bytes = src.as_bytes();
    let mut j = i;
    while j < bytes.len() && bytes[j] != b'\n' {
        j += 1;
    }
    j
}

fn skip_block_comment(src: &str, i: usize) -> usize {
    match src[i + 2..].find("*/") {
        Some(rel) => i + 2 + rel + 2,
        None => src.len(),
    }
}

fn skip_ws_and_comments(src: &str, mut i: usize, lang: Lang) -> usize {
    let bytes = src.as_bytes();
    loop {
        if i >= bytes.len() {
            return i;
        }
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if lang == Lang::Python && bytes[i] == b'#' {
            i = skip_line_comment(src, i);
            continue;
        }
        if lang != Lang::Python && bytes[i] == b'/' && src[i..].starts_with("//") {
            i = skip_line_comment(src, i);
            continue;
        }
        if lang != Lang::Python && bytes[i] == b'/' && src[i..].starts_with("/*") {
            i = skip_block_comment(src, i);
            continue;
        }
        return i;
    }
}

fn skip_group(src: &str, open: usize, lang: Lang) -> usize {
    let bytes = src.as_bytes();
    if open >= bytes.len() {
        return open;
    }
    let mut depth = 0i32;
    let mut i = open;
    while i < bytes.len() {
        if let Some(token) = try_string(src, i, lang) {
            i = token.end;
            continue;
        }
        match bytes[i] {
            b'(' | b'[' | b'{' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' | b'}' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => i += utf8_len(bytes[i]),
        }
    }
    i
}

fn read_ident(src: &str, i: usize) -> Option<(usize, &str)> {
    let bytes = src.as_bytes();
    let first = *bytes.get(i)?;
    if !is_ident_start(first) {
        return None;
    }
    let mut j = i + 1;
    while j < bytes.len() && is_ident_cont(bytes[j]) {
        j += 1;
    }
    Some((j, &src[i..j]))
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$'
}

fn is_ident_cont(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

fn utf8_len(byte: u8) -> usize {
    if byte & 0x80 == 0 {
        1
    } else if byte & 0xE0 == 0xC0 {
        2
    } else if byte & 0xF0 == 0xE0 {
        3
    } else {
        4
    }
}
