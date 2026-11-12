//! Light/dark mode and the app-wide monospace font.

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use serde::{Deserialize, Serialize};

use crate::config;

/// Preferred monospace families, first installed wins. When none is, the
/// theme's own resolved mono font (always an installed family) is used.
const MONO_FONTS: &[&str] = &["JetBrains Mono", "Adwaita Mono", "DejaVu Sans Mono"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    Light,
    Dark,
}

impl From<Appearance> for ThemeMode {
    fn from(value: Appearance) -> Self {
        match value {
            Appearance::Light => ThemeMode::Light,
            Appearance::Dark => ThemeMode::Dark,
        }
    }
}

/// Applies the saved appearance at startup (light when none is saved or
/// the config can't be read; the sidebar reports config errors).
pub fn init(cx: &mut App) {
    let saved = config::load().ok().and_then(|c| c.appearance).unwrap_or_default();
    apply(saved, cx);
}

pub fn current(cx: &App) -> Appearance {
    if Theme::global(cx).is_dark() {
        Appearance::Dark
    } else {
        Appearance::Light
    }
}

/// Switches mode and persists it. A failed save only loses the choice for
/// the next launch, so it doesn't block the switch.
pub fn set(appearance: Appearance, cx: &mut App) -> anyhow::Result<()> {
    apply(appearance, cx);
    let mut cfg = config::load()?;
    cfg.appearance = Some(appearance);
    config::save(&cfg)
}

fn apply(appearance: Appearance, cx: &mut App) {
    // Changing mode reloads the theme config, fonts included, so the font
    // override has to be reapplied afterwards every time.
    Theme::change(appearance, None, cx);
    let installed = cx.text_system().all_font_names();
    let mono = MONO_FONTS
        .iter()
        .find(|f| installed.iter().any(|i| i == *f))
        .map(|f| SharedString::from(*f))
        .unwrap_or_else(|| Theme::global(cx).mono_font_family.clone());
    Theme::update(cx, |theme| {
        theme.font_family = mono.clone();
        theme.mono_font_family = mono;
        theme.font_size = px(14.);
    });
}
