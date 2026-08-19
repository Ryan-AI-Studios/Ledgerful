use regex::Regex;
use std::sync::OnceLock;

fn static_regex(lock: &'static OnceLock<Regex>, pattern: &'static str) -> &'static Regex {
    lock.get_or_init(|| Regex::new(pattern).expect("static ask_routing regex must compile"))
}

#[derive(Debug)]
pub enum ExactIntent {
    CallersOf(String),
    CalleesOf(String),
    RouteOwner(String),
    ListRoutes,
    SymbolDefinition(String),
}

pub fn parse_intent(query: &str) -> Option<ExactIntent> {
    let lower = query.to_lowercase();
    let q = query.trim();

    // what calls X / show callers of X / who calls X
    static RE_WHAT_WHO_CALLS: OnceLock<Regex> = OnceLock::new();
    if let Some(caps) = static_regex(
        &RE_WHAT_WHO_CALLS,
        r"(?i)^(?:what|who) calls ([a-zA-Z0-9_:]+)",
    )
    .captures(q)
    {
        return Some(ExactIntent::CallersOf(caps[1].to_string()));
    }
    static RE_SHOW_CALLERS: OnceLock<Regex> = OnceLock::new();
    if let Some(caps) =
        static_regex(&RE_SHOW_CALLERS, r"(?i)^show callers of ([a-zA-Z0-9_:]+)").captures(q)
    {
        return Some(ExactIntent::CallersOf(caps[1].to_string()));
    }
    static RE_FIND_CALLERS: OnceLock<Regex> = OnceLock::new();
    if let Some(caps) =
        static_regex(&RE_FIND_CALLERS, r"(?i)^find callers of ([a-zA-Z0-9_:]+)").captures(q)
    {
        return Some(ExactIntent::CallersOf(caps[1].to_string()));
    }

    // what does X call / show callees of X
    static RE_WHAT_DOES_CALL: OnceLock<Regex> = OnceLock::new();
    if let Some(caps) =
        static_regex(&RE_WHAT_DOES_CALL, r"(?i)^what does ([a-zA-Z0-9_:]+) call").captures(q)
    {
        return Some(ExactIntent::CalleesOf(caps[1].to_string()));
    }
    static RE_SHOW_CALLEES: OnceLock<Regex> = OnceLock::new();
    if let Some(caps) =
        static_regex(&RE_SHOW_CALLEES, r"(?i)^show callees of ([a-zA-Z0-9_:]+)").captures(q)
    {
        return Some(ExactIntent::CalleesOf(caps[1].to_string()));
    }

    // list route handlers / list routes
    if lower.contains("list route handlers")
        || lower.contains("list routes")
        || lower.contains("show routes")
        || lower.contains("find all axum route handlers")
        || lower.contains("what routes are defined")
    {
        return Some(ExactIntent::ListRoutes);
    }

    // which handler owns route Y
    static RE_WHICH_HANDLER: OnceLock<Regex> = OnceLock::new();
    if let Some(caps) =
        static_regex(&RE_WHICH_HANDLER, r"(?i)which handler owns route ([\w/-]+)").captures(q)
    {
        return Some(ExactIntent::RouteOwner(caps[1].to_string()));
    }
    static RE_WHO_HANDLES: OnceLock<Regex> = OnceLock::new();
    if let Some(caps) =
        static_regex(&RE_WHO_HANDLES, r"(?i)who handles route ([\w/-]+)").captures(q)
    {
        return Some(ExactIntent::RouteOwner(caps[1].to_string()));
    }
    static RE_HANDLER_FOR: OnceLock<Regex> = OnceLock::new();
    if let Some(caps) =
        static_regex(&RE_HANDLER_FOR, r"(?i)handler for route ([\w/-]+)").captures(q)
    {
        return Some(ExactIntent::RouteOwner(caps[1].to_string()));
    }

    // where is symbol X defined
    static RE_WHERE_DEFINED: OnceLock<Regex> = OnceLock::new();
    if let Some(caps) = static_regex(
        &RE_WHERE_DEFINED,
        r"(?i)where is (?:symbol )?([a-zA-Z0-9_:]+) defined",
    )
    .captures(q)
    {
        return Some(ExactIntent::SymbolDefinition(caps[1].to_string()));
    }
    static RE_FIND_DEFINITION: OnceLock<Regex> = OnceLock::new();
    if let Some(caps) = static_regex(
        &RE_FIND_DEFINITION,
        r"(?i)^find definition of ([a-zA-Z0-9_:]+)",
    )
    .captures(q)
    {
        return Some(ExactIntent::SymbolDefinition(caps[1].to_string()));
    }

    // 0142: locate / find-symbol shapes → SymbolDefinition
    // (docs/operator-surface-policy.md §2 — structured sources before LLM).
    // Parse order is load-bearing: ListRoutes / callers / callees / route-owner /
    // "where is … defined" / "find definition of" run first so bare `find X`
    // never steals `find all axum route handlers` or `find callers of X`.
    // Deliberately **no** `where is X` without "defined" (false-positive magnet).
    if !has_conceptual_locate_tokens(q) {
        if let Some(sym) = parse_find_type_qualified(q) {
            return Some(ExactIntent::SymbolDefinition(sym));
        }
        if let Some(sym) = parse_locate_symbol(q) {
            return Some(ExactIntent::SymbolDefinition(sym));
        }
        if let Some(sym) = parse_bare_find_symbol(q) {
            return Some(ExactIntent::SymbolDefinition(sym));
        }
    }

    None
}

/// Whole-word conceptual / narrative tokens that must not map to locate intents.
fn has_conceptual_locate_tokens(q: &str) -> bool {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(how|why|way|best|explain|architect|architecture|implement|implementation)\b",
        )
        .ok()
    });
    re.as_ref().is_some_and(|r| r.is_match(q))
}

/// Identifier class for locate/find targets: `^[A-Za-z_][A-Za-z0-9_:]*$`.
fn is_code_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

/// Word count after stripping punctuation (for bare-find ≤8 guard).
fn locate_query_word_count(q: &str) -> usize {
    let stripped = strip_trailing_punct(q.trim());
    stripped
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
        .filter(|w| !w.is_empty())
        .count()
}

/// `find (the )?(function|fn|method|struct|type|trait|enum|symbol) X`
fn parse_find_type_qualified(q: &str) -> Option<String> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?i)^find\s+(?:the\s+)?(?:function|fn|method|struct|type|trait|enum|symbol)\s+([A-Za-z_][A-Za-z0-9_:]*)\s*[?.!]*$",
        )
        .ok()
    });
    let re = re.as_ref()?;
    let caps = re.captures(q.trim())?;
    let sym = caps.get(1)?.as_str();
    if is_code_identifier(sym) {
        Some(sym.to_string())
    } else {
        None
    }
}

/// `locate (the )?(function|…|symbol)? X`
fn parse_locate_symbol(q: &str) -> Option<String> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?i)^locate\s+(?:the\s+)?(?:(?:function|fn|method|struct|type|trait|enum|symbol)\s+)?([A-Za-z_][A-Za-z0-9_:]*)\s*[?.!]*$",
        )
        .ok()
    });
    let re = re.as_ref()?;
    let caps = re.captures(q.trim())?;
    let sym = caps.get(1)?.as_str();
    if is_code_identifier(sym) {
        Some(sym.to_string())
    } else {
        None
    }
}

/// Bare `find X` / `find X in the codebase` when X is a single code identifier
/// and the query has ≤ 8 words after punctuation strip.
fn parse_bare_find_symbol(q: &str) -> Option<String> {
    let q = q.trim();
    if locate_query_word_count(q) > 8 {
        return None;
    }
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)^find\s+([A-Za-z_][A-Za-z0-9_:]*)(?:\s+in\s+the\s+codebase)?\s*[?.!]*$")
            .ok()
    });
    let re = re.as_ref()?;
    let caps = re.captures(q)?;
    let sym = caps.get(1)?.as_str();
    if is_code_identifier(sym) {
        Some(sym.to_string())
    } else {
        None
    }
}

// --- Product-docs routing (0139 Ask Docs Grounding) ---
//
// Product-usage / Daily 5 / agent-default-path questions are answered from
// the tracked skill SoT (`docs/Ledgerful/skill.md`) plus live clap `about`
// text — never free-form LLM invention. Policy:
// `docs/operator-surface-policy.md` §2 (Structured sources before LLM synthesis).
// Wire order in `execute_ask` is normative: CG-F20 → ProductDocs → CG-F31 → LLM
// so "session start commands" is not swallowed by GenericDiscovery.

/// Product-docs intent class. Daily 5 is the load-bearing first member;
/// AgentDefaultPath is a synonym that yields the same answer.
#[derive(Debug, PartialEq, Eq)]
pub enum ProductDocsIntent {
    /// "What is the Daily 5?", "agent default path", "session start commands".
    Daily5,
}

fn product_docs_trigger() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(daily\s*5|daily\s*five|agent\s+default\s+path|session\s+start\s+commands)\b",
        )
        .ok()
    })
    .as_ref()
}

fn product_docs_impl_exclude() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    // Only true implementation words — do not exclude natural product
    // phrasing like "how does Daily 5 work".
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(compute|computes|calculation|calculations|calculate|calculates|score|scores|internal|internals|implement|implements|implementation|algorithm)\b",
        )
        .ok()
    })
    .as_ref()
}

/// Strip trailing punctuation so "Daily 5?" / "Daily 5!" still match product phrases.
fn strip_trailing_punct(s: &str) -> &str {
    s.trim_end_matches(|c: char| {
        matches!(
            c,
            '?' | '!' | '.' | ',' | ';' | ':' | '\'' | '"' | '”' | '’'
        )
    })
}

/// Recognizes product-docs / Daily 5 / agent-default-path phrasing.
/// Conservative: returns `None` for implementation questions, pure CG-F20
/// structural shapes, and plain command-discovery without product phrases.
///
/// Policy: `docs/operator-surface-policy.md` §2 (Structured sources before LLM synthesis).
pub fn parse_product_docs_intent(query: &str) -> Option<ProductDocsIntent> {
    let q = strip_trailing_punct(query.trim());
    if q.is_empty() {
        return None;
    }

    // Implementation-flavored questions fall through even if they mention Daily 5.
    if product_docs_impl_exclude().is_some_and(|re| re.is_match(q)) {
        return None;
    }

    if product_docs_trigger().is_some_and(|re| re.is_match(q)) {
        return Some(ProductDocsIntent::Daily5);
    }

    None
}

/// Distinguishes the two command-discovery answer shapes CG-F31 handles.
#[derive(Debug, PartialEq, Eq)]
pub enum CommandDiscoveryIntent {
    /// "what commands show repo health?" and similar phrasings: answer from
    /// the curated `REPO_HEALTH_COMMANDS` list.
    RepoHealth,
    /// A generic "what command does/shows X" question that doesn't match the
    /// repo-health topic; answered via keyword overlap against the full
    /// live corpus.
    GenericDiscovery,
}

/// Recognizes operator-intent command-discovery phrasing. Returns `None`
/// (graceful low-confidence fallback, spec requirement #7) when the query
/// doesn't look like a command-discovery question at all -- in particular,
/// implementation-flavored questions ("how does X work", "what does
/// calculate_hotspots do") must not match.
pub fn parse_command_discovery_intent(query: &str) -> Option<CommandDiscoveryIntent> {
    let q = query.trim();

    // If the query is implementation-flavored, it must fall through
    static RE_IMPL_KEYWORDS: OnceLock<Regex> = OnceLock::new();
    let impl_keywords = static_regex(
        &RE_IMPL_KEYWORDS,
        r"(?i)\b(compute|computes|calculation|calculations|calculate|calculates|score|scores|internal|internals|implement|implements|implementation|work|works|code|structure|algorithm)\b",
    );
    if impl_keywords.is_match(q) {
        return None;
    }

    static RE_REPO_HEALTH_TRIGGER: OnceLock<Regex> = OnceLock::new();
    let repo_health_trigger = static_regex(
        &RE_REPO_HEALTH_TRIGGER,
        r"(?i)what\s+command|which\s+command|how\s+do\s+i\s+check|how\s+can\s+i\s+check",
    );
    // The repo/project/CLI qualifier is mandatory here (no trailing `?`):
    // without it, "status"/"health" alone match any implementation-flavored
    // question (e.g. "how do I check the status of my database connection"),
    // which must fall through to `None` per spec requirement #7. The
    // `\bcommands?\b` alternative mirrors `discovery_shape` below, so "what
    // commands show repo health" style phrasing that mentions "command(s)"
    // still qualifies even without an explicit repo-qualifier word.
    static RE_REPO_HEALTH_TOPIC: OnceLock<Regex> = OnceLock::new();
    let repo_health_topic = static_regex(
        &RE_REPO_HEALTH_TOPIC,
        r"(?i)\b(repo|repository|project|ledgerful|ledgerful|cli)\b\s*(health|status|current\s+state|project\s+status)|\bcommands?\b.*\b(health|status|current\s+state|project\s+status)\b",
    );

    if repo_health_trigger.is_match(q) && repo_health_topic.is_match(q) {
        return Some(CommandDiscoveryIntent::RepoHealth);
    }

    // Generic command-discovery shape: must explicitly be about commands
    // (contains "command"/"commands") and phrased as a discovery question
    // ("what/which command(s) show/does/handles/runs ..."). This is
    // intentionally conservative: plain "how does X work" or "what does X
    // do" (without the word "command") must fall through unaffected so the
    // CG-F20 structural path and narrative/implementation questions are
    // unaffected.
    static RE_MENTIONS_COMMAND: OnceLock<Regex> = OnceLock::new();
    let mentions_command = static_regex(&RE_MENTIONS_COMMAND, r"(?i)\bcommands?\b");
    static RE_DISCOVERY_SHAPE: OnceLock<Regex> = OnceLock::new();
    let discovery_shape = static_regex(
        &RE_DISCOVERY_SHAPE,
        r"(?i)\b(what|which|how)\b.*\bcommands?\b|\bcommands?\b.*\b(show|shows|does|do|handle|handles|run|runs|list|lists)\b",
    );

    if mentions_command.is_match(q) && discovery_shape.is_match(q) {
        return Some(CommandDiscoveryIntent::GenericDiscovery);
    }

    None
}
