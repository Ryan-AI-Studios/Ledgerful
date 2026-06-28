use camino::Utf8Path;
use ledgerful::index::languages::Language;
use ledgerful::index::metrics::{ComplexityResult, ComplexityScorer, NativeComplexityScorer};

#[test]
fn rust_complexity_scores_nested_control_flow() {
    let source = r#"
        fn simple() {
            println!("hello");
        }

        fn complex(x: i32) {
            if x > 0 {
                for i in 0..x {
                    if i % 2 == 0 {
                        println!("{}", i);
                    }
                }
            } else {
                match x {
                    -1 => println!("one"),
                    _ => println!("other"),
                }
            }
        }
    "#;

    let scorer = NativeComplexityScorer::new();
    let result = scorer
        .score_file(Utf8Path::new("test.rs"), source, Language::Rust)
        .unwrap();

    assert_eq!(result.functions.len(), 2);

    let simple = result
        .functions
        .iter()
        .find(|f| f.name == "simple")
        .unwrap();
    assert_eq!(simple.cyclomatic, 1);
    assert_eq!(simple.cognitive, 0);

    let complex = result
        .functions
        .iter()
        .find(|f| f.name == "complex")
        .unwrap();
    assert_eq!(complex.cyclomatic, 6);
    assert_eq!(complex.cognitive, 10);
}

#[test]
fn python_complexity_scores_indentation_depth() {
    let source = r#"
def simple():
    print("hello")

def complex(x):
    if x > 0:
        for i in range(x):
            if i % 2 == 0:
                print(i)
    else:
        print("negative")
    "#;

    let scorer = NativeComplexityScorer::new();
    let result = scorer
        .score_file(Utf8Path::new("test.py"), source, Language::Python)
        .unwrap();

    assert_eq!(result.functions.len(), 2);
    let complex = result
        .functions
        .iter()
        .find(|f| f.name == "complex")
        .unwrap();
    assert_eq!(complex.cyclomatic, 4);
    assert_eq!(complex.cognitive, 6);
}

#[test]
fn typescript_complexity_scores_ts_syntax() {
    let source = r#"
function simple() {
  return 1;
}

function complex(value: number) {
  if (value > 10) {
    for (const item of [1, 2, 3]) {
      if (item === value) {
        return item;
      }
    }
  }
  return value > 0 ? value : 0;
}
    "#;

    let scorer = NativeComplexityScorer::new();
    let result = scorer
        .score_file(Utf8Path::new("test.ts"), source, Language::TypeScript)
        .unwrap();

    assert_eq!(result.functions.len(), 2);
    let complex = result
        .functions
        .iter()
        .find(|f| f.name == "complex")
        .unwrap();
    assert_eq!(complex.cyclomatic, 5);
    assert_eq!(complex.cognitive, 7);
}

#[test]
fn test_syntax_error_marks_ast_incomplete() {
    let source = "fn broken( { if true {";
    let scorer = NativeComplexityScorer::new();
    let result = scorer
        .score_file(Utf8Path::new("broken.rs"), source, Language::Rust)
        .unwrap();

    assert!(result.ast_incomplete);
    assert!(!result.complexity_capped);
}

#[test]
fn test_unsupported_language_is_not_applicable() {
    let scorer = NativeComplexityScorer::new();
    let result = scorer
        .score_supported_path(Utf8Path::new("README.md"), "# title")
        .unwrap();

    assert!(matches!(result, ComplexityResult::NotApplicable { .. }));
}

#[test]
fn test_large_file_caps_complexity() {
    let source = "fn a() {}\n".repeat(10_001);
    let scorer = NativeComplexityScorer::new();
    let result = scorer
        .score_file(Utf8Path::new("large.rs"), &source, Language::Rust)
        .unwrap();

    assert!(result.complexity_capped);
    assert!(result.functions.is_empty());
}
