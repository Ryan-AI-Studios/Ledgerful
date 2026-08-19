mod answers;
mod parse;

pub use answers::{
    CommandSurface, LOCAL_GROUNDING_MISS_BANNER, LOCAL_GROUNDING_SEARCH_BANNER,
    PRODUCT_DOCS_DAILY5_BANNER, build_command_corpus, build_command_discovery_answer,
    build_daily5_answer, resolve_intent,
};
pub(crate) use answers::{
    format_local_grounding_miss, format_search_evidence, search_symbol_secondary,
};
pub use parse::{
    CommandDiscoveryIntent, ExactIntent, ProductDocsIntent, parse_command_discovery_intent,
    parse_intent, parse_product_docs_intent,
};

#[cfg(test)]
mod tests;
