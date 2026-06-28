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
    pub snippet: Option<String>,
    pub highlighted: Option<String>,
    pub line_number: Option<usize>,
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
            let mut highlighted_opt = None;
            let mut line_number_opt = None;

            if let Some(content_val) = retrieved_doc
                .get_first(content_field)
                .and_then(|v| v.as_str())
            {
                let snippet = snippet_generator.snippet_from_doc(&retrieved_doc);
                let highlighted_html = snippet.to_html();
                if !highlighted_html.is_empty() {
                    snippet_opt = Some(snippet.fragment().to_string());
                    highlighted_opt = Some(
                        highlighted_html
                            .replace("<b>", "\x1b[1m")
                            .replace("</b>", "\x1b[0m"),
                    );
                    let plain_snippet = snippet.fragment();
                    if let Some(idx) = content_val.find(plain_snippet) {
                        let lines_before =
                            content_val[..idx].chars().filter(|&c| c == '\n').count();
                        line_number_opt = Some(lines_before + 1);
                    } else {
                        line_number_opt = Some(1);
                    }
                }
            }

            results.push(SearchResult {
                path,
                line_count,
                score,
                snippet: snippet_opt,
                highlighted: highlighted_opt,
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
            let mut highlighted_opt = None;
            let mut line_number_opt = None;

            if let Some(content_val) = retrieved_doc
                .get_first(content_field)
                .and_then(|v| v.as_str())
            {
                let snippet = snippet_generator.snippet_from_doc(&retrieved_doc);
                let highlighted_html = snippet.to_html();
                if !highlighted_html.is_empty() {
                    snippet_opt = Some(snippet.fragment().to_string());
                    highlighted_opt = Some(
                        highlighted_html
                            .replace("<b>", "\x1b[1m")
                            .replace("</b>", "\x1b[0m"),
                    );
                    let plain_snippet = snippet.fragment();
                    if let Some(idx) = content_val.find(plain_snippet) {
                        let lines_before =
                            content_val[..idx].chars().filter(|&c| c == '\n').count();
                        line_number_opt = Some(lines_before + 1);
                    } else {
                        line_number_opt = Some(1);
                    }
                }
            }

            results.push(SearchResult {
                path,
                line_count,
                score,
                snippet: snippet_opt,
                highlighted: highlighted_opt,
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
        }
    }
}

pub struct CodeIdentifierTokenStream<'a> {
    text: &'a str,
    chars: Vec<(usize, char)>,
    index: usize,
    token: Token,
}

impl TokenStream for CodeIdentifierTokenStream<'_> {
    fn advance(&mut self) -> bool {
        if self.index >= self.chars.len() {
            return false;
        }

        while self.index < self.chars.len() && !self.chars[self.index].1.is_alphanumeric() {
            self.index += 1;
        }

        if self.index >= self.chars.len() {
            return false;
        }

        let start = self.chars[self.index].0;
        let first_char = self.chars[self.index].1;
        self.index += 1;

        let mut prev_char = first_char;
        while self.index < self.chars.len() {
            let curr_char = self.chars[self.index].1;

            if !curr_char.is_alphanumeric() {
                break;
            }

            if prev_char.is_lowercase() && curr_char.is_uppercase() {
                break;
            }

            if prev_char.is_uppercase()
                && curr_char.is_uppercase()
                && self.index + 1 < self.chars.len()
            {
                let next_char = self.chars[self.index + 1].1;
                if next_char.is_lowercase() {
                    break;
                }
            }

            prev_char = curr_char;
            self.index += 1;
        }

        let end_idx = if self.index < self.chars.len() {
            self.chars[self.index].0
        } else {
            self.text.len()
        };

        self.token.offset_from = start;
        self.token.offset_to = end_idx;
        self.token.text = self.text[start..end_idx].to_string();
        self.token.position = self.token.position.wrapping_add(1);
        true
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
}
