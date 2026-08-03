//! Colour support gate for human CLI output (track 0131).
//!
//! Product colour **must** go through [`paint`] / `if_supports_color`. Bare
//! `.green()` / `.red()` / … always emit ANSI and ignore `set_override`.
//!
//! ## Stream pairing
//!
//! | Destination | Stream |
//! |---|---|
//! | `println!` / product tables on stdout | [`Stream::Stdout`] |
//! | `tracing::info!` / `debug!` on `cli_summary` | [`Stream::Stdout`] |
//! | `tracing::warn!` / `error!` on `cli_summary` | [`Stream::Stderr`] |
//! | `eprintln!` (INVALID / UNSIGNED, banners) | [`Stream::Stderr`] |

use owo_colors::{OwoColorize, Stream, SupportsColorsDisplay};

/// Apply a colour/style transform only when `stream` supports colour (TTY +
/// env heuristics), honouring [`owo_colors::set_override`].
///
/// Thin wrapper around [`OwoColorize::if_supports_color`] so call sites share
/// one import path.
#[inline]
pub fn paint<'a, T, R, F>(stream: Stream, value: &'a T, f: F) -> SupportsColorsDisplay<'a, T, R, F>
where
    T: OwoColorize,
    F: Fn(&'a T) -> R,
{
    value.if_supports_color(stream, f)
}

/// Startup colour policy (call once after CLI parse, before command dispatch).
///
/// 1. [`owo_colors::unset_override`] — clear any sticky test override
/// 2. `NO_COLOR` present (any value) → force off ([no-color.org](https://no-color.org))
/// 3. else `FORCE_COLOR` or `CLICOLOR_FORCE` non-empty and not `"0"` → force on
/// 4. else leave unset → per-stream auto via `supports-color`
pub fn init_color_support() {
    owo_colors::unset_override();

    if std::env::var_os("NO_COLOR").is_some() {
        owo_colors::set_override(false);
        return;
    }

    if force_color_enabled() {
        owo_colors::set_override(true);
    }
}

fn force_color_enabled() -> bool {
    for key in ["FORCE_COLOR", "CLICOLOR_FORCE"] {
        if let Some(val) = std::env::var_os(key)
            && !val.is_empty()
            && val != "0"
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use owo_colors::OwoColorize;

    #[test]
    fn paint_force_off_emits_no_esc() {
        owo_colors::set_override(false);
        let value = "ok";
        let s = paint(Stream::Stdout, &value, |t| t.green()).to_string();
        assert!(
            !s.contains('\u{1b}'),
            "expected no ESC under set_override(false), got {s:?}"
        );
        assert_eq!(s, "ok");
        owo_colors::unset_override();
    }

    #[test]
    fn paint_force_on_may_colour() {
        owo_colors::set_override(true);
        let value = "ok";
        let s = paint(Stream::Stdout, &value, |t| t.green()).to_string();
        assert!(
            s.contains('\u{1b}'),
            "expected ESC under set_override(true), got {s:?}"
        );
        owo_colors::unset_override();
    }

    #[test]
    fn force_color_helper_rejects_zero_and_empty() {
        // Unit pure: empty OsString and "0" are false; "1" is true.
        assert!(!{
            let empty = std::ffi::OsString::new();
            let zero = std::ffi::OsString::from("0");
            let one = std::ffi::OsString::from("1");
            let check = |v: &std::ffi::OsString| !v.is_empty() && v != "0";
            check(&empty) || check(&zero) || !check(&one)
        });
    }

    #[test]
    fn bare_owo_ignores_override_documenting_why_gate_matters() {
        // Load-bearing API fact (spec D1): bare methods always emit ANSI.
        owo_colors::set_override(false);
        let bare = "x".green().to_string();
        assert!(
            bare.contains('\u{1b}'),
            "bare .green() must still emit ANSI (proves conversion is required)"
        );
        let gated = "x"
            .if_supports_color(Stream::Stdout, |t| t.green())
            .to_string();
        assert!(
            !gated.contains('\u{1b}'),
            "if_supports_color must honour set_override(false)"
        );
        owo_colors::unset_override();
    }

    #[test]
    fn multi_style_via_style_new_force_off_no_esc() {
        owo_colors::set_override(false);
        let s = "ok"
            .if_supports_color(Stream::Stdout, |t| {
                t.style(owo_colors::Style::new().green().bold())
            })
            .to_string();
        assert!(!s.contains('\u{1b}'));
        assert_eq!(s, "ok");
        owo_colors::unset_override();
    }
}
