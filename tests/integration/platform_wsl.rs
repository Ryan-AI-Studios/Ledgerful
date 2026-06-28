#[test]
fn test_wsl_path_classification_seam() {
    #[cfg(target_os = "linux")]
    {
        use ledgerful::platform::detect::is_wsl;
        use ledgerful::platform::paths::{PathKind, classify_path};

        if is_wsl() {
            assert_eq!(classify_path("/mnt/c/Users/Admin"), PathKind::WslMounted);
            assert_eq!(classify_path("/home/user"), PathKind::Native);
        }
    }
}
