use std::ffi::OsString;

/// RAII guard for temporarily mutating a process environment variable.
///
/// On construction the original value (or lack thereof) is captured, and on
/// drop the variable is restored to that original state.
///
/// # Safety
///
/// `std::env::set_var` and `std::env::remove_var` are `unsafe` in Rust 2024
/// because concurrent mutation of the process environment block is UB. Callers
/// must serialize tests that use this guard (for example via
/// `#[serial_test::serial(env)]`). The `unsafe` blocks are intentionally
/// isolated to this single module so they are not scattered across the test
/// suite.
pub struct TempEnv {
    key: String,
    original: Option<OsString>,
}

impl TempEnv {
    /// Set an environment variable to `value` for the lifetime of the guard.
    #[allow(dead_code)]
    pub fn set(key: &str, value: &str) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: env mutation is serialized via #[serial(env)] on callers.
        unsafe {
            std::env::set_var(key, value);
        }
        Self {
            key: key.to_string(),
            original,
        }
    }

    /// Remove an environment variable for the lifetime of the guard.
    #[allow(dead_code)]
    pub fn remove(key: &str) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: env mutation is serialized via #[serial(env)] on callers.
        unsafe {
            std::env::remove_var(key);
        }
        Self {
            key: key.to_string(),
            original,
        }
    }
}

impl Drop for TempEnv {
    fn drop(&mut self) {
        match &self.original {
            Some(val) => {
                // SAFETY: env mutation is serialized via #[serial(env)] on callers.
                unsafe {
                    std::env::set_var(&self.key, val);
                }
            }
            None => {
                // SAFETY: env mutation is serialized via #[serial(env)] on callers.
                unsafe {
                    std::env::remove_var(&self.key);
                }
            }
        }
    }
}
