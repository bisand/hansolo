//! daisyUI's `dim` and `light`, as Denise themes, and following the system.
//!
//! Denise derives a theme from nine seeds and computes the rest for contrast.
//! Here the rest is not derived but *copied*: every surface and content colour
//! below is daisyUI 5's own, converted from OKLCH to sRGB once, so the dashboard
//! looks like the daisyUI theme it is named after rather than a relative of it.
//! [`validate`] still checks what that gives up in contrast.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use denise::theme::{AA_LARGE, ColorScheme, Metrics};
use denise::{Color, Role, Theme};
use hansolo_core::ThemePreference;

const fn rgb(v: u32) -> Color {
    Color::from_rgb888(v)
}

/// daisyUI 5 `dim`: `--radius-box: 1rem`, `--radius-field: 0.5rem`, `--depth: 0`.
pub const DIM: Theme = Theme::from_seeds(
    "dim",
    ColorScheme::Dark,
    rgb(0x2A303C),
    rgb(0x9FE88D),
    rgb(0xFF7D5D),
    rgb(0xC792E9),
    rgb(0x1C212B),
    rgb(0x28EBFF),
    rgb(0x62EFBD),
    rgb(0xEFD057),
    rgb(0xFFAE9B),
)
.with_color(Role::Base200, rgb(0x242933))
.with_color(Role::Base300, rgb(0x20252E))
.with_color(Role::BaseContent, rgb(0xB2CCD6))
.with_color(Role::PrimaryContent, rgb(0x091307))
.with_color(Role::SecondaryContent, rgb(0x160503))
.with_color(Role::AccentContent, rgb(0x0E0813))
.with_color(Role::NeutralContent, rgb(0xB2CCD6))
.with_color(Role::InfoContent, rgb(0x011316))
.with_color(Role::SuccessContent, rgb(0x03140D))
.with_color(Role::WarningContent, rgb(0x141003))
.with_color(Role::ErrorContent, rgb(0x160B09))
.with_metrics(Metrics {
    radius_selector: 16,
    radius_field: 8,
    radius_box: 16,
    ..Metrics::DEFAULT
})
.with_depth(0);

/// daisyUI 5 `light`: `--radius-box: 0.5rem`, `--radius-field: 0.25rem`, `--depth: 1`.
pub const LIGHT: Theme = Theme::from_seeds(
    "light",
    ColorScheme::Light,
    rgb(0xFFFFFF),
    rgb(0x422AD5),
    rgb(0xF43098),
    rgb(0x00D3BB),
    rgb(0x09090B),
    rgb(0x00BAFE),
    rgb(0x00D390),
    rgb(0xFCB700),
    rgb(0xFF627D),
)
.with_color(Role::Base200, rgb(0xF8F8F8))
.with_color(Role::Base300, rgb(0xEEEEEE))
.with_color(Role::BaseContent, rgb(0x18181B))
.with_color(Role::PrimaryContent, rgb(0xE0E7FF))
.with_color(Role::SecondaryContent, rgb(0xF9E4F0))
.with_color(Role::AccentContent, rgb(0x084D49))
.with_color(Role::NeutralContent, rgb(0xE4E4E7))
.with_color(Role::InfoContent, rgb(0x042E49))
.with_color(Role::SuccessContent, rgb(0x004C39))
.with_color(Role::WarningContent, rgb(0x793205))
.with_color(Role::ErrorContent, rgb(0x4D0218))
.with_metrics(Metrics {
    radius_selector: 8,
    radius_field: 4,
    radius_box: 8,
    ..Metrics::DEFAULT
})
.with_depth(1);

/// Checks the copied palettes at the large-text floor.
///
/// daisyUI picks its content colours by eye in OKLCH, not by WCAG arithmetic,
/// so AA for body text is not guaranteed; the dashboard's small text sits on
/// base surfaces, which clear AA comfortably in both themes.
pub fn validate() {
    for theme in [DIM, LIGHT] {
        if let Err(failure) = theme.validate(AA_LARGE) {
            eprintln!(
                "theme {}: {:?} on {:?} is {:.2}:1",
                theme.name,
                failure.content,
                failure.surface,
                failure.ratio_x100 as f32 / 100.0
            );
        }
    }
}

/// What the operating system says, sampled off the UI thread.
///
/// Asking can mean a D-Bus round trip on Linux, which is not something a frame
/// should wait on, so a thread asks every couple of seconds and the UI reads an
/// atomic.
#[derive(Clone)]
pub struct SystemTheme {
    /// 0 = unknown, 1 = dark, 2 = light.
    mode: Arc<AtomicU8>,
}

impl SystemTheme {
    /// Starts sampling. On a machine with no desktop there is nothing to ask,
    /// and the answer stays unknown.
    pub fn start(windowed: bool) -> Self {
        let mode = Arc::new(AtomicU8::new(0));
        #[cfg(feature = "desktop")]
        if windowed {
            let shared = mode.clone();
            let _ = std::thread::Builder::new()
                .name("system-theme".into())
                .spawn(move || {
                    loop {
                        let value = match dark_light::detect() {
                            Ok(dark_light::Mode::Dark) => 1,
                            Ok(dark_light::Mode::Light) => 2,
                            _ => 0,
                        };
                        shared.store(value, Ordering::Relaxed);
                        std::thread::sleep(std::time::Duration::from_secs(2));
                    }
                });
        }
        #[cfg(not(feature = "desktop"))]
        let _ = windowed;
        Self { mode }
    }

    /// The theme for a preference, falling back to dark when the system cannot
    /// be read — which includes every bare display.
    pub fn resolve(&self, preference: ThemePreference) -> Theme {
        match preference {
            ThemePreference::Dark => DIM,
            ThemePreference::Light => LIGHT,
            ThemePreference::System => match self.mode.load(Ordering::Relaxed) {
                2 => LIGHT,
                _ => DIM,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_text_is_readable_in_both_themes() {
        for theme in [DIM, LIGHT] {
            for surface in [Role::Base100, Role::Base200, Role::Base300] {
                let ratio =
                    denise::theme::contrast_x100(theme.color(surface), theme.color(Role::BaseContent));
                assert!(ratio >= denise::theme::AA, "{} {surface:?}: {ratio}", theme.name);
            }
        }
    }

    #[test]
    fn system_falls_back_to_dark() {
        let system = SystemTheme {
            mode: Arc::new(AtomicU8::new(0)),
        };
        assert_eq!(system.resolve(ThemePreference::System).name, "dim");
        assert_eq!(system.resolve(ThemePreference::Light).name, "light");
    }
}
