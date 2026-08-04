use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::snippet::SnippetGenerator;
use tantivy::tokenizer::{
    LowerCaser, TextAnalyzer, Token, TokenStream, Tokenizer, WhitespaceTokenizer,
};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

#[derive(Serialize, Deserialize, Debug)]
pub struct SearchResult {
    pub path: String,
    pub line_count: usize,
    pub score: f32,
    /// Plain-text snippet fragment (no HTML, no ANSI).
    pub snippet: Option<String>,
    /// Byte ranges into `snippet` that match the query. Callers apply emphasis
    /// at print time; the engine never embeds escape sequences or HTML.
    pub highlight_ranges: Option<Vec<(usize, usize)>>,
    pub line_number: Option<usize>,
}

/// Max plain-snippet bytes kept after our own truncation arithmetic.
/// Width/ellipsis policy is a separate track; this only prevents mid-char slices.
const SNIPPET_MAX_BYTES: usize = 240;

/// Plain fragment plus byte highlight ranges into that fragment.
struct BuiltSnippet {
    fragment: String,
    /// Inclusive-start exclusive-end byte ranges into `fragment`.
    ranges: Vec<(usize, usize)>,
    line_number: Option<usize>,
}

/// Truncate `s` to at most `max_bytes` without splitting a multi-byte character.
pub(crate) fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Build a plain fragment + byte highlight ranges from a tantivy Snippet.
/// Uses `fragment()` + `highlighted()` ranges only — no HTML round-trip.
/// Ranges are already aligned to the fragment (byte offsets).
fn build_snippet(snippet: &tantivy::snippet::Snippet, content: &str) -> Option<BuiltSnippet> {
    let raw_fragment = snippet.fragment();
    if raw_fragment.is_empty() {
        return None;
    }

    // Track-owned truncation: never slice mid-character (DoD-6).
    let fragment = truncate_at_char_boundary(raw_fragment, SNIPPET_MAX_BYTES).to_string();
    let frag_len = fragment.len();

    let mut ranges: Vec<(usize, usize)> = snippet
        .highlighted()
        .iter()
        .filter_map(|r| {
            let start = r.start;
            let end = r.end;
            if start > end || end > frag_len {
                return None;
            }
            // Defensive: tantivy ranges are byte-aligned, but guard our slices.
            if !fragment.is_char_boundary(start) || !fragment.is_char_boundary(end) {
                return None;
            }
            Some((start, end))
        })
        .collect();
    // Deterministic order for same repo state.
    ranges.sort_by_key(|(s, e)| (*s, *e));

    let line_number = if let Some(idx) = content.find(&fragment) {
        if content.is_char_boundary(idx) {
            let lines_before = content[..idx].chars().filter(|&c| c == '\n').count();
            Some(lines_before + 1)
        } else {
            Some(1)
        }
    } else if let Some(idx) = content.find(raw_fragment) {
        // Fragment may have been truncated; locate via the full raw fragment.
        if content.is_char_boundary(idx) {
            let lines_before = content[..idx].chars().filter(|&c| c == '\n').count();
            Some(lines_before + 1)
        } else {
            Some(1)
        }
    } else {
        Some(1)
    };

    Some(BuiltSnippet {
        fragment,
        ranges,
        line_number,
    })
}

pub struct TantivySearchEngine {
    index: Index,
    reader: IndexReader,
    schema: Schema,
}

impl TantivySearchEngine {
    pub fn open_or_create(path: &Path) -> Result<Self> {
        let mut schema_builder = Schema::builder();
        schema_builder.add_text_field("path", TEXT | STORED);
        schema_builder.add_u64_field("line_count", STORED);
        schema_builder.add_text_field("language", TEXT | STORED);

        schema_builder.add_text_field(
            "trigrams",
            TextOptions::default().set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("code_trigram")
                    .set_index_option(IndexRecordOption::Basic),
            ),
        );

        schema_builder.add_text_field(
            "content",
            TextOptions::default().set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("code")
                    .set_index_option(IndexRecordOption::WithFreqsAndPositions),
            ) | STORED,
        );

        let schema = schema_builder.build();

        if !path.exists() {
            std::fs::create_dir_all(path).into_diagnostic()?;
        }

        let index = match Index::open_or_create(
            tantivy::directory::MmapDirectory::open(path).into_diagnostic()?,
            schema.clone(),
        ) {
            Ok(idx) => idx,
            Err(tantivy::TantivyError::SchemaError(e)) => {
                tracing::warn!(
                    "Tantivy schema mismatch detected: {}. Re-initializing search index...",
                    e
                );
                // Clear index directory
                let _ = std::fs::remove_dir_all(path);
                let _ = std::fs::create_dir_all(path);
                Index::open_or_create(
                    tantivy::directory::MmapDirectory::open(path).into_diagnostic()?,
                    schema.clone(),
                )
                .into_diagnostic()?
            }
            Err(e) => return Err(e).into_diagnostic(),
        };

        index.tokenizers().register(
            "code_trigram",
            TextAnalyzer::builder(WhitespaceTokenizer::default())
                .filter(LowerCaser)
                .build(),
        );

        index.tokenizers().register(
            "code",
            TextAnalyzer::builder(CodeIdentifierTokenizer)
                .filter(LowerCaser)
                .build(),
        );

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .into_diagnostic()?;

        Ok(Self {
            index,
            reader,
            schema,
        })
    }

    pub fn get_writer(&self, memory_budget_bytes: usize) -> Result<IndexWriter> {
        self.index.writer(memory_budget_bytes).into_diagnostic()
    }

    pub fn reload_reader(&self) -> Result<()> {
        self.reader.reload().into_diagnostic()
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    pub fn document_count(&self) -> usize {
        self.reader.searcher().num_docs() as usize
    }

    pub fn search(&self, query_str: &str, limit: usize) -> Result<Vec<SearchResult>> {
        let searcher = self.reader.searcher();
        let content_field = self.schema.get_field("content").into_diagnostic()?;
        let path_field = self.schema.get_field("path").into_diagnostic()?;
        let trigrams_field = self.schema.get_field("trigrams").into_diagnostic()?;
        let line_count_field = self.schema.get_field("line_count").into_diagnostic()?;

        // 1. Trigram Pre-filtering
        // If the query is alphanumeric (likely a symbol or keyword), use trigrams to prune noisy matches.
        let mut pre_filter_query: Option<Box<dyn tantivy::query::Query>> = None;
        if query_str.len() >= 3 && query_str.chars().all(|c| c.is_alphanumeric() || c == '_') {
            let tgrams = crate::search::trigram::extract_trigrams(query_str);
            if !tgrams.is_empty() {
                let mut subqueries: Vec<(tantivy::query::Occur, Box<dyn tantivy::query::Query>)> =
                    Vec::new();
                for t in tgrams {
                    let term = Term::from_field_text(trigrams_field, &t.to_lowercase());
                    subqueries.push((
                        tantivy::query::Occur::Must,
                        Box::new(tantivy::query::TermQuery::new(
                            term,
                            IndexRecordOption::Basic,
                        )),
                    ));
                }
                pre_filter_query = Some(Box::new(tantivy::query::BooleanQuery::new(subqueries)));
            }
        }

        // 2. Standard BM25 Ranking
        let query_parser = QueryParser::for_index(&self.index, vec![content_field, path_field]);
        let bm25_query = query_parser.parse_query(query_str).into_diagnostic()?;

        // Combined query: (Trigrams MUST match) AND (BM25 ranking)
        let final_query: Box<dyn tantivy::query::Query> = if let Some(trigram_q) = pre_filter_query
        {
            Box::new(tantivy::query::BooleanQuery::new(vec![
                (tantivy::query::Occur::Must, trigram_q),
                (tantivy::query::Occur::Must, bm25_query),
            ]))
        } else {
            bm25_query
        };

        let snippet_generator =
            SnippetGenerator::create(&searcher, &*final_query, content_field).into_diagnostic()?;

        let top_docs = searcher
            .search(&final_query, &TopDocs::with_limit(limit).order_by_score())
            .into_diagnostic()?;

        let mut results = Vec::new();
        for (score, doc_address) in top_docs {
            let retrieved_doc: TantivyDocument = searcher.doc(doc_address).into_diagnostic()?;

            let path = retrieved_doc
                .get_first(path_field)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();

            let line_count = retrieved_doc
                .get_first(line_count_field)
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;

            let mut snippet_opt = None;
            let mut ranges_opt = None;
            let mut line_number_opt = None;

            if let Some(content_val) = retrieved_doc
                .get_first(content_field)
                .and_then(|v| v.as_str())
            {
                let snippet = snippet_generator.snippet_from_doc(&retrieved_doc);
                if let Some(built) = build_snippet(&snippet, content_val) {
                    snippet_opt = Some(built.fragment);
                    ranges_opt = Some(built.ranges);
                    line_number_opt = built.line_number;
                }
            }

            results.push(SearchResult {
                path,
                line_count,
                score,
                snippet: snippet_opt,
                highlight_ranges: ranges_opt,
                line_number: line_number_opt,
            });
        }

        Ok(results)
    }

    pub fn search_fuzzy(&self, query_str: &str, limit: usize) -> Result<Vec<SearchResult>> {
        use tantivy::query::FuzzyTermQuery;

        let searcher = self.reader.searcher();
        let content_field = self.schema.get_field("content").into_diagnostic()?;
        let path_field = self.schema.get_field("path").into_diagnostic()?;
        let line_count_field = self.schema.get_field("line_count").into_diagnostic()?;

        let term = Term::from_field_text(content_field, &query_str.to_lowercase());
        let fuzzy_query = Box::new(FuzzyTermQuery::new(term, 2, true));

        let snippet_generator =
            SnippetGenerator::create(&searcher, &*fuzzy_query, content_field).into_diagnostic()?;

        let top_docs = searcher
            .search(&*fuzzy_query, &TopDocs::with_limit(limit).order_by_score())
            .into_diagnostic()?;

        let mut results = Vec::new();
        for (score, doc_address) in top_docs {
            let retrieved_doc: TantivyDocument = searcher.doc(doc_address).into_diagnostic()?;

            let path = retrieved_doc
                .get_first(path_field)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();

            let line_count = retrieved_doc
                .get_first(line_count_field)
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;

            let mut snippet_opt = None;
            let mut ranges_opt = None;
            let mut line_number_opt = None;

            if let Some(content_val) = retrieved_doc
                .get_first(content_field)
                .and_then(|v| v.as_str())
            {
                let snippet = snippet_generator.snippet_from_doc(&retrieved_doc);
                if let Some(built) = build_snippet(&snippet, content_val) {
                    snippet_opt = Some(built.fragment);
                    ranges_opt = Some(built.ranges);
                    line_number_opt = built.line_number;
                }
            }

            results.push(SearchResult {
                path,
                line_count,
                score,
                snippet: snippet_opt,
                highlight_ranges: ranges_opt,
                line_number: line_number_opt,
            });
        }

        Ok(results)
    }

    pub fn search_trigrams(&self, trigrams: &[String], limit: usize) -> Result<Vec<String>> {
        use tantivy::query::BooleanQuery;
        use tantivy::query::TermQuery;

        let searcher = self.reader.searcher();
        let trigrams_field = self.schema.get_field("trigrams").into_diagnostic()?;
        let path_field = self.schema.get_field("path").into_diagnostic()?;

        let mut subqueries: Vec<(tantivy::query::Occur, Box<dyn tantivy::query::Query>)> =
            Vec::new();
        for trigram in trigrams {
            let lower = trigram.to_lowercase();
            let term = Term::from_field_text(trigrams_field, &lower);
            let query = TermQuery::new(term, IndexRecordOption::Basic);
            subqueries.push((tantivy::query::Occur::Must, Box::new(query)));
        }

        if subqueries.is_empty() {
            return Ok(Vec::new());
        }

        let query = BooleanQuery::new(subqueries);
        let top_docs = searcher
            .search(&query, &TopDocs::with_limit(limit).order_by_score())
            .into_diagnostic()?;

        let mut results = Vec::new();
        for (_score, doc_address) in top_docs {
            let retrieved_doc: TantivyDocument = searcher.doc(doc_address).into_diagnostic()?;
            let path = retrieved_doc
                .get_first(path_field)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            results.push(path);
        }

        Ok(results)
    }

    pub fn all_paths(&self, limit: usize) -> Result<Vec<String>> {
        use tantivy::query::AllQuery;

        let searcher = self.reader.searcher();
        let path_field = self.schema.get_field("path").into_diagnostic()?;

        let top_docs = searcher
            .search(&AllQuery, &TopDocs::with_limit(limit).order_by_score())
            .into_diagnostic()?;

        let mut results = Vec::new();
        for (_score, doc_address) in top_docs {
            let retrieved_doc: TantivyDocument = searcher.doc(doc_address).into_diagnostic()?;
            let path = retrieved_doc
                .get_first(path_field)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            results.push(path);
        }

        Ok(results)
    }

    pub fn clear(&self) -> Result<()> {
        let mut writer = self.get_writer(50_000_000)?;
        writer.delete_all_documents().into_diagnostic()?;
        writer.commit().into_diagnostic()?;
        Ok(())
    }

    pub fn segment_count(&self) -> Result<usize> {
        let searcher = self.reader.searcher();
        Ok(searcher.segment_readers().len())
    }

    pub fn verify_index_integrity(&self, index_path: &Path) -> Result<()> {
        let meta_path = index_path.join("meta.json");
        if !meta_path.exists() {
            return Err(miette::miette!(
                "Index meta.json missing at {:?}",
                meta_path
            ));
        }

        let meta_content = std::fs::read_to_string(&meta_path).into_diagnostic()?;
        let meta: serde_json::Value = serde_json::from_str(&meta_content).into_diagnostic()?;

        let segments = meta
            .get("segments")
            .and_then(|v| v.as_array())
            .ok_or_else(|| miette::miette!("Malformed meta.json: 'segments' field missing"))?;

        for segment in segments {
            let id = segment
                .get("segment_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| miette::miette!("Malformed meta.json: 'segment_id' missing"))?;

            let clean_id = id.replace("-", "");
            let store_file = index_path.join(format!("{}.store", clean_id));

            if !store_file.exists() {
                let mut files = Vec::new();
                if let Ok(entries) = std::fs::read_dir(index_path) {
                    for entry in entries.flatten() {
                        files.push(entry.file_name().to_string_lossy().to_string());
                    }
                }
                return Err(miette::miette!(
                    "Tantivy segment file missing: {:?}. The index is corrupt or incomplete. Files in directory: {:?}",
                    store_file,
                    files
                ));
            }
        }

        Ok(())
    }
}

#[derive(Clone)]
pub struct CodeIdentifierTokenizer;

impl Tokenizer for CodeIdentifierTokenizer {
    type TokenStream<'a> = CodeIdentifierTokenStream<'a>;
    fn token_stream<'a>(&mut self, text: &'a str) -> Self::TokenStream<'a> {
        CodeIdentifierTokenStream {
            text,
            chars: text.char_indices().collect(),
            index: 0,
            token: Token::default(),
            pending: std::collections::VecDeque::new(),
            next_position: 0,
        }
    }
}

/// Pending dual-emit / multi-piece token ready to surface from [`advance`].
struct PendingToken {
    text: String,
    offset_from: usize,
    offset_to: usize,
    position: usize,
    position_length: usize,
}

pub struct CodeIdentifierTokenStream<'a> {
    text: &'a str,
    chars: Vec<(usize, char)>,
    index: usize,
    token: Token,
    pending: std::collections::VecDeque<PendingToken>,
    next_position: usize,
}

impl CodeIdentifierTokenStream<'_> {
    fn emit_pending(&mut self, pending: PendingToken) {
        self.token.offset_from = pending.offset_from;
        self.token.offset_to = pending.offset_to;
        self.token.text = pending.text;
        self.token.position = pending.position;
        self.token.position_length = pending.position_length;
    }

    fn take_next_position(&mut self) -> usize {
        let pos = self.next_position;
        self.next_position = self.next_position.wrapping_add(1);
        pos
    }

    /// True if `c` may start or continue a code identifier (alnum or `_` glue).
    fn is_ident_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    /// Split pure-alnum span with existing camelCase / PascalCase rules.
    fn enqueue_camel_case_pieces(&mut self, start_char: usize, end_char: usize) {
        let mut i = start_char;
        while i < end_char {
            let piece_start_char = i;
            let first_char = self.chars[i].1;
            i += 1;
            let mut prev_char = first_char;
            while i < end_char {
                let curr_char = self.chars[i].1;

                if prev_char.is_lowercase() && curr_char.is_uppercase() {
                    break;
                }

                if prev_char.is_uppercase() && curr_char.is_uppercase() && i + 1 < end_char {
                    let next_char = self.chars[i + 1].1;
                    if next_char.is_lowercase() {
                        break;
                    }
                }

                prev_char = curr_char;
                i += 1;
            }

            let offset_from = self.chars[piece_start_char].0;
            let offset_to = if i < self.chars.len() {
                self.chars[i].0
            } else {
                self.text.len()
            };
            let position = self.take_next_position();
            self.pending.push_back(PendingToken {
                text: self.text[offset_from..offset_to].to_string(),
                offset_from,
                offset_to,
                position,
                position_length: 1,
            });
        }
    }

    /// Dual-emit full underscore identifier + non-empty `_`-parts.
    fn enqueue_underscore_dual(&mut self, offset_from: usize, offset_to: usize) {
        let full = &self.text[offset_from..offset_to];
        let mut parts: Vec<(usize, usize)> = Vec::new();
        let mut part_start = offset_from;
        for (rel, ch) in full.char_indices() {
            if ch == '_' {
                let part_end = offset_from + rel;
                if part_end > part_start {
                    parts.push((part_start, part_end));
                }
                part_start = part_end + ch.len_utf8();
            }
        }
        if offset_to > part_start {
            parts.push((part_start, offset_to));
        }

        let num_parts = parts.len();
        let full_position_length = if num_parts == 0 { 1 } else { num_parts };

        let full_pos = self.take_next_position();
        self.pending.push_back(PendingToken {
            text: full.to_string(),
            offset_from,
            offset_to,
            position: full_pos,
            position_length: full_position_length,
        });

        for (p_from, p_to) in parts {
            let position = self.take_next_position();
            self.pending.push_back(PendingToken {
                text: self.text[p_from..p_to].to_string(),
                offset_from: p_from,
                offset_to: p_to,
                position,
                position_length: 1,
            });
        }
    }
}

impl TokenStream for CodeIdentifierTokenStream<'_> {
    fn advance(&mut self) -> bool {
        if let Some(pending) = self.pending.pop_front() {
            self.emit_pending(pending);
            return true;
        }

        // Skip non-identifier characters. `_` is identifier glue (may start ids).
        while self.index < self.chars.len() && !Self::is_ident_char(self.chars[self.index].1) {
            self.index += 1;
        }

        if self.index >= self.chars.len() {
            return false;
        }

        let start_char = self.index;
        let offset_from = self.chars[start_char].0;

        // Collect maximal identifier (alnum + `_`).
        while self.index < self.chars.len() && Self::is_ident_char(self.chars[self.index].1) {
            self.index += 1;
        }

        let end_char = self.index;
        let offset_to = if end_char < self.chars.len() {
            self.chars[end_char].0
        } else {
            self.text.len()
        };

        let ident = &self.text[offset_from..offset_to];
        if ident.contains('_') {
            self.enqueue_underscore_dual(offset_from, offset_to);
        } else {
            // Pure alphanumeric: camelCase / PascalCase splits only (no dual full).
            self.enqueue_camel_case_pieces(start_char, end_char);
        }

        if let Some(pending) = self.pending.pop_front() {
            self.emit_pending(pending);
            true
        } else {
            false
        }
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.token
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::trigram::extract_trigrams;
    use tempfile::TempDir;

    #[test]
    fn truncate_at_char_boundary_ascii() {
        assert_eq!(truncate_at_char_boundary("hello world", 5), "hello");
        assert_eq!(truncate_at_char_boundary("short", 100), "short");
    }

    #[test]
    fn truncate_at_char_boundary_cjk_and_emoji() {
        // "中" is 3 bytes; max_bytes=4 would land mid-"文" if sliced blind.
        let s = "中文emoji😀boundary";
        let t = truncate_at_char_boundary(s, 4);
        assert!(t.is_char_boundary(t.len()));
        assert!(!t.contains('\u{FFFD}'));
        // 4 bytes → only first CJK char (3 bytes), not mid-second-char.
        assert_eq!(t, "中");

        // 4-byte emoji: landing max_bytes inside the emoji must not split it.
        let with_emoji = "ab😀cd";
        // 'a','b' = 2 bytes, emoji = 4 bytes → at max 3 we keep "ab"
        let t2 = truncate_at_char_boundary(with_emoji, 3);
        assert_eq!(t2, "ab");
        assert!(t2.is_char_boundary(t2.len()));
    }

    fn make_engine(dir: &TempDir) -> TantivySearchEngine {
        TantivySearchEngine::open_or_create(dir.path()).expect("open_or_create")
    }

    fn index_doc(engine: &TantivySearchEngine, path: &str, content: &str) {
        let schema = engine.schema();
        let path_field = schema.get_field("path").expect("path field");
        let content_field = schema.get_field("content").expect("content field");
        let line_count_field = schema.get_field("line_count").expect("line_count field");
        let trigrams_field = schema.get_field("trigrams").expect("trigrams field");

        let tgrams_str = extract_trigrams(content)
            .into_iter()
            .collect::<Vec<_>>()
            .join(" ");

        let mut writer = engine.get_writer(15_000_000).expect("writer");
        let mut doc = TantivyDocument::default();
        doc.add_text(path_field, path);
        doc.add_text(content_field, content);
        doc.add_u64(line_count_field, 1);
        doc.add_text(trigrams_field, &tgrams_str);
        writer.add_document(doc).expect("add_document");
        writer.commit().expect("commit");
        engine.reload_reader().expect("reload_reader");
    }

    #[test]
    fn trigram_search_finds_underscore_identifier() {
        let dir = TempDir::new().expect("tempdir");
        let engine = make_engine(&dir);
        index_doc(
            &engine,
            "src/search/regex_filter.rs",
            "fn execute_scan() {}",
        );
        let tgrams: Vec<String> = extract_trigrams("execute_scan").into_iter().collect();
        let results = engine
            .search_trigrams(&tgrams, 10)
            .expect("search_trigrams");
        assert!(!results.is_empty());
    }

    #[test]
    fn trigram_search_finds_storage_cozo() {
        let dir = TempDir::new().expect("tempdir");
        let engine = make_engine(&dir);
        index_doc(&engine, "src/state/cozo.rs", "struct storage_cozo {}");
        let tgrams: Vec<String> = extract_trigrams("storage_cozo").into_iter().collect();
        let results = engine
            .search_trigrams(&tgrams, 10)
            .expect("search_trigrams");
        assert!(!results.is_empty());
    }

    #[test]
    fn trigram_search_finds_non_underscore_pattern() {
        let dir = TempDir::new().expect("tempdir");
        let engine = make_engine(&dir);
        index_doc(&engine, "src/main.rs", "struct MainRunner {}");
        let tgrams: Vec<String> = extract_trigrams("MainRunner").into_iter().collect();
        let results = engine
            .search_trigrams(&tgrams, 10)
            .expect("search_trigrams");
        assert!(!results.is_empty());
    }

    fn collect_tokens(text: &str) -> Vec<Token> {
        let mut tokenizer = CodeIdentifierTokenizer;
        let mut stream = tokenizer.token_stream(text);
        let mut out = Vec::new();
        while stream.advance() {
            out.push(stream.token().clone());
        }
        out
    }

    #[test]
    fn code_identifier_dual_emit_snake_case() {
        let tokens = collect_tokens("verify_step_key");
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert!(
            texts.contains(&"verify_step_key"),
            "expected full token, got {texts:?}"
        );
        assert!(texts.contains(&"verify"), "expected part verify: {texts:?}");
        assert!(texts.contains(&"step"), "expected part step: {texts:?}");
        assert!(texts.contains(&"key"), "expected part key: {texts:?}");

        let full = tokens
            .iter()
            .find(|t| t.text == "verify_step_key")
            .expect("full");
        assert_eq!(full.position_length, 3);
        assert_eq!(full.offset_from, 0);
        assert_eq!(full.offset_to, "verify_step_key".len());

        let parts: Vec<&Token> = tokens
            .iter()
            .filter(|t| matches!(t.text.as_str(), "verify" | "step" | "key"))
            .collect();
        assert_eq!(parts.len(), 3);
        for p in &parts {
            assert_eq!(p.position_length, 1);
            assert_eq!(&text_slice(p), p.text.as_str());
        }

        // Sequential positions per emitted token.
        let mut positions: Vec<usize> = tokens.iter().map(|t| t.position).collect();
        positions.sort();
        assert_eq!(positions, (0..tokens.len()).collect::<Vec<_>>());
        // Full first, then parts in order.
        assert_eq!(tokens[0].text, "verify_step_key");
        assert_eq!(tokens[0].position, 0);
        assert_eq!(tokens[1].text, "verify");
        assert_eq!(tokens[1].position, 1);
        assert_eq!(tokens[2].text, "step");
        assert_eq!(tokens[2].position, 2);
        assert_eq!(tokens[3].text, "key");
        assert_eq!(tokens[3].position, 3);
    }

    fn text_slice(t: &Token) -> String {
        // Offsets validated against the source used in dual-emit test.
        "verify_step_key"[t.offset_from..t.offset_to].to_string()
    }

    #[test]
    fn code_identifier_camel_case_preserved() {
        let tokens = collect_tokens("MainRunner");
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["Main", "Runner"]);
        for t in &tokens {
            assert_eq!(t.position_length, 1);
        }
        assert_eq!(tokens[0].position, 0);
        assert_eq!(tokens[1].position, 1);
    }

    #[test]
    fn code_identifier_pure_alphanumeric_single() {
        let tokens = collect_tokens("hello");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].text, "hello");
        assert_eq!(tokens[0].position_length, 1);
        assert_eq!(tokens[0].position, 0);
    }

    #[test]
    fn code_identifier_leading_underscore() {
        let tokens = collect_tokens("_private");
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert!(texts.contains(&"_private"));
        assert!(texts.contains(&"private"));
        let full = tokens.iter().find(|t| t.text == "_private").expect("full");
        assert_eq!(full.position_length, 1);
    }

    #[test]
    fn bm25_search_finds_full_snake_case_identifier() {
        let dir = TempDir::new().expect("tempdir");
        let engine = make_engine(&dir);
        index_doc(&engine, "src/verify/bayesian.rs", "fn verify_step_key() {}");
        let results = engine
            .search("verify_step_key", 10)
            .expect("search full id");
        assert!(
            !results.is_empty(),
            "BM25 must find full snake_case identifier after dual-emit"
        );

        // Dual-emit parts should still hit (DoD-2: "step_key" or "verify").
        // QueryParser ANDs dual-emitted terms, so a shorter compound query
        // "step_key" also dual-emits a full term never indexed alone when only
        // "verify_step_key" was present; single-part queries always work.
        let part_verify = engine.search("verify", 10).expect("search verify");
        assert!(!part_verify.is_empty(), "part verify should hit");

        let part_step = engine.search("step", 10).expect("search step");
        assert!(!part_step.is_empty(), "part step should hit");
    }
}
