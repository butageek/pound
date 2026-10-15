//! Theme selection: the mode (Auto follows the system; Light/Dark are
//! forced) plus the palette used in each mode. All of it lives on the
//! settings page and persists under `HKCU\Software\Pound` (see
//! `register::load_settings` / `register::save_setting`).

/// The theme mode. Persisted as `Theme`.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub enum Theme {
    #[default]
    Auto,
    Light,
    Dark,
}

impl Theme {
    pub fn as_str(&self) -> &'static str {
        match self {
            Theme::Auto => "auto",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    pub fn parse(value: &str) -> Option<Theme> {
        match value.trim() {
            "auto" => Some(Theme::Auto),
            "light" => Some(Theme::Light),
            "dark" => Some(Theme::Dark),
            _ => None,
        }
    }

    /// The resolved background color for pre-paint surfaces (window and
    /// WebView2, both set at build time): the chosen palette's `--bg`,
    /// with Auto resolved against `system_dark` the way the shell's
    /// `matchMedia` head script resolves it after paint.
    pub fn resolved_background(
        &self,
        light: LightPalette,
        dark: DarkPalette,
        system_dark: bool,
    ) -> [u8; 3] {
        match (*self, system_dark) {
            (Theme::Dark, _) | (Theme::Auto, true) => dark.background(),
            (Theme::Light, _) | (Theme::Auto, false) => light.background(),
        }
    }
}

/// The palette used while the resolved mode is light.
/// Persisted as `LightPalette`.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub enum LightPalette {
    #[default]
    Solarized,
    Latte,
}

impl LightPalette {
    pub fn as_str(&self) -> &'static str {
        match self {
            LightPalette::Solarized => "solarized",
            LightPalette::Latte => "latte",
        }
    }

    pub fn parse(value: &str) -> Option<LightPalette> {
        match value.trim() {
            "solarized" => Some(LightPalette::Solarized),
            "latte" => Some(LightPalette::Latte),
            _ => None,
        }
    }

    /// The palette's `--bg` from the shell CSS, as RGB — used for the
    /// window and WebView2 backgrounds before first paint (see
    /// `view::run`), so dark themes never flash white. Keep in sync
    /// with SHELL_HTML (pinned by `palette_backgrounds_match_the_css`).
    pub fn background(&self) -> [u8; 3] {
        match self {
            LightPalette::Solarized => [0xfd, 0xf6, 0xe3],
            LightPalette::Latte => [0xef, 0xf1, 0xf5],
        }
    }
}

/// The palette used while the resolved mode is dark.
/// Persisted as `DarkPalette`.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub enum DarkPalette {
    #[default]
    OneDark,
    Mocha,
}

impl DarkPalette {
    pub fn as_str(&self) -> &'static str {
        match self {
            DarkPalette::OneDark => "one-dark",
            DarkPalette::Mocha => "mocha",
        }
    }

    pub fn parse(value: &str) -> Option<DarkPalette> {
        match value.trim() {
            "one-dark" => Some(DarkPalette::OneDark),
            "mocha" => Some(DarkPalette::Mocha),
            _ => None,
        }
    }

    /// The palette's `--bg` from the shell CSS, as RGB (see
    /// `LightPalette::background`).
    pub fn background(&self) -> [u8; 3] {
        match self {
            DarkPalette::OneDark => [0x28, 0x2c, 0x34],
            DarkPalette::Mocha => [0x1e, 0x1e, 0x2e],
        }
    }
}

/// The reading font size (px) shared by the rendered and source panes.
/// The Settings slider offers anything in this range (0.5 steps);
/// persisted as `FontSize`, anything outside falls back to the default.
pub const MIN_FONT_SIZE: f32 = 10.0;
pub const MAX_FONT_SIZE: f32 = 28.0;
pub const DEFAULT_FONT_SIZE: f32 = 15.5;

/// Parse a persisted size (the slider's value, e.g. "15.5"); `None`
/// unless it is within `MIN_FONT_SIZE..=MAX_FONT_SIZE`.
pub fn parse_font_size(value: &str) -> Option<f32> {
    let px = value.trim().parse::<f32>().ok()?;
    (MIN_FONT_SIZE..=MAX_FONT_SIZE).contains(&px).then_some(px)
}

/// All persisted appearance settings, injected into the shell before
/// first paint (see `shell_html` in the view; the palette/font fields
/// are read there — Windows-only, hence the relaxed lint off-Windows).
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug)]
pub struct Settings {
    pub theme: Theme,
    pub light: LightPalette,
    pub dark: DarkPalette,
    /// Chosen font family for all reading/editing surfaces; `None` =
    /// the default stacks. Persisted as `Font` (free-form; the view
    /// drops names that are no longer installed).
    pub font: Option<String>,
    /// Reading font size in px; `DEFAULT_FONT_SIZE` unless set.
    pub font_size: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: Theme::default(),
            light: LightPalette::default(),
            dark: DarkPalette::default(),
            font: None,
            font_size: DEFAULT_FONT_SIZE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_backgrounds_match_the_shell_css() {
        // Pinned to the --bg values in SHELL_HTML; if a palette changes,
        // update both — the pre-paint background must blend seamlessly.
        assert_eq!(LightPalette::Solarized.background(), [0xfd, 0xf6, 0xe3]);
        assert_eq!(LightPalette::Latte.background(), [0xef, 0xf1, 0xf5]);
        assert_eq!(DarkPalette::OneDark.background(), [0x28, 0x2c, 0x34]);
        assert_eq!(DarkPalette::Mocha.background(), [0x1e, 0x1e, 0x2e]);
    }

    #[test]
    fn auto_resolves_the_pre_paint_background() {
        let latte = LightPalette::Latte;
        let mocha = DarkPalette::Mocha;
        assert_eq!(
            Theme::Light.resolved_background(latte, mocha, true),
            latte.background()
        );
        assert_eq!(
            Theme::Dark.resolved_background(latte, mocha, false),
            mocha.background()
        );
        assert_eq!(
            Theme::Auto.resolved_background(latte, mocha, true),
            mocha.background()
        );
        assert_eq!(
            Theme::Auto.resolved_background(latte, mocha, false),
            latte.background()
        );
    }

    #[test]
    fn theme_values_round_trip() {
        for theme in [Theme::Auto, Theme::Light, Theme::Dark] {
            assert_eq!(Theme::parse(theme.as_str()), Some(theme));
        }
        for palette in [LightPalette::Solarized, LightPalette::Latte] {
            assert_eq!(LightPalette::parse(palette.as_str()), Some(palette));
        }
        for palette in [DarkPalette::OneDark, DarkPalette::Mocha] {
            assert_eq!(DarkPalette::parse(palette.as_str()), Some(palette));
        }
    }

    #[test]
    fn unknown_values_do_not_parse() {
        assert_eq!(Theme::parse("solarized"), None);
        assert_eq!(LightPalette::parse("mocha"), None);
        assert_eq!(DarkPalette::parse("latte"), None);
        assert_eq!(Theme::parse(""), None);
        assert_eq!(Settings::default().theme, Theme::Auto);
    }

    #[test]
    fn font_size_values_round_trip() {
        for size in [MIN_FONT_SIZE, 13.5, DEFAULT_FONT_SIZE, 20.0, MAX_FONT_SIZE] {
            assert_eq!(parse_font_size(&size.to_string()), Some(size));
        }
        assert_eq!(Settings::default().font_size, DEFAULT_FONT_SIZE);
    }

    #[test]
    fn out_of_range_font_sizes_do_not_parse() {
        assert_eq!(parse_font_size("9.5"), None); // just below the slider
        assert_eq!(parse_font_size("28.5"), None); // just above it
        assert_eq!(parse_font_size("0"), None); // the v0.11.0 invisible-text bug
        assert_eq!(parse_font_size("garbage"), None);
        assert_eq!(parse_font_size(""), None);
    }
}
