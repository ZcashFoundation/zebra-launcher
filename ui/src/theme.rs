//! Light and dark appearance: resolves the setting against the system preference and applies it
//! to the document.
//!
//! The stylesheet defines every colour as a token on `:root` (the dark set) and redefines them
//! under `:root[data-theme="light"]`, so applying a theme is one attribute on `<html>`.

use launcher_types::Theme;
use leptos::prelude::*;
use wasm_bindgen::{JsCast, closure::Closure};

use crate::app::AppState;

/// The label and explanation of an appearance, for the Settings radios.
pub fn theme_text(theme: Theme) -> (&'static str, &'static str) {
    match theme {
        Theme::System => (
            "System",
            "Follow the operating system's light or dark setting.",
        ),
        Theme::Light => ("Light", "Always light."),
        Theme::Dark => ("Dark", "Always dark."),
    }
}

/// The `value` of an appearance radio, the same word as on the wire.
pub fn theme_value(theme: Theme) -> &'static str {
    match theme {
        Theme::System => "system",
        Theme::Light => "light",
        Theme::Dark => "dark",
    }
}

/// Whether the appearance is dark for a setting and the system's preference.
pub fn resolve(theme: Theme, system_prefers_light: bool) -> bool {
    match theme {
        Theme::System => !system_prefers_light,
        Theme::Light => false,
        Theme::Dark => true,
    }
}

/// The `data-theme` value and `<meta name="color-scheme">` content for an appearance.
pub fn scheme_name(dark: bool) -> &'static str {
    if dark { "dark" } else { "light" }
}

/// Follows the appearance for the page's life: the theme being previewed in Settings, else the
/// stored one, resolved against `prefers-color-scheme`, which is watched for changes. Until the
/// settings load the app follows the system preference.
pub fn init(state: &AppState) {
    let state = *state;
    let system_prefers_light = RwSignal::new(false);
    if let Some(query) = web_sys::window()
        .and_then(|window| window.match_media("(prefers-color-scheme: light)").ok())
        .flatten()
    {
        system_prefers_light.set(query.matches());
        // The listener lives as long as the page; the closure is deliberately leaked.
        let on_change = Closure::<dyn Fn(web_sys::MediaQueryListEvent)>::new(
            move |event: web_sys::MediaQueryListEvent| system_prefers_light.set(event.matches()),
        );
        let _ =
            query.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref());
        on_change.forget();
    }

    Effect::new(move |_| {
        let theme = state
            .preview_theme
            .get()
            .or_else(|| {
                state
                    .settings
                    .with(|settings| settings.as_ref().map(|s| s.theme))
            })
            .unwrap_or_default();
        let dark = resolve(theme, system_prefers_light.get());
        state.resolved_dark.set(dark);
        apply(dark);
    });
}

/// Sets `data-theme` on `<html>` and the `color-scheme` meta tag, so form controls and
/// scrollbars follow too.
fn apply(dark: bool) {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    if let Some(root) = document.document_element() {
        let _ = root.set_attribute("data-theme", scheme_name(dark));
    }
    if let Ok(Some(meta)) = document.query_selector("meta[name=\"color-scheme\"]") {
        let _ = meta.set_attribute("content", scheme_name(dark));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_follows_the_preference_and_fixed_themes_ignore_it() {
        assert!(resolve(Theme::System, false));
        assert!(!resolve(Theme::System, true));
        assert!(!resolve(Theme::Light, false));
        assert!(!resolve(Theme::Light, true));
        assert!(resolve(Theme::Dark, false));
        assert!(resolve(Theme::Dark, true));
    }

    #[test]
    fn theme_words() {
        assert_eq!(theme_text(Theme::System).0, "System");
        assert_eq!(theme_value(Theme::System), "system");
        assert_eq!(theme_value(Theme::Light), "light");
        assert_eq!(theme_value(Theme::Dark), "dark");
    }

    #[test]
    fn scheme_names() {
        assert_eq!(scheme_name(true), "dark");
        assert_eq!(scheme_name(false), "light");
    }
}
