#[test]
fn test_windows_platform_and_path_seams() {
    #[cfg(target_os = "windows")]
    {
        use ledgerful::platform::detect::{PlatformType, current_platform};
        use ledgerful::platform::paths::{PathKind, classify_path};

        assert_eq!(current_platform(), PlatformType::Windows);
        assert_eq!(classify_path(r"C:\Users\Admin"), PathKind::Native);
        assert_eq!(classify_path(r"\\server\share"), PathKind::Network);
    }
}
