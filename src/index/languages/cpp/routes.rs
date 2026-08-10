use crate::index::routes::ExtractedRoute;
use crate::index::symbols::Symbol;
use miette::Result;

/// C++ HTTP route frameworks are out of scope for v1 (D4) — empty Ok.
pub fn extract_routes(_content: &str, _symbols: &[Symbol]) -> Result<Vec<ExtractedRoute>> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_empty_ok() {
        let routes = extract_routes("int main() {}", &[]).unwrap();
        assert!(routes.is_empty());
    }
}
