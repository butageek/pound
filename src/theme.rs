//! Theme selection: the mode (Auto follows the system; Light/Dark are
//! forced) plus the palette used in each mode. All of it lives on the
//! settings page and persists under `HKCU\Software\Pound` (see
//! `register::load_settings` / `register::save_setting`).

/// The theme mode. Persisted as `Theme`.
#[derive(Debug, Default, PartialEq, Eq)]
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
}

/// The palette used while the resolved mode is light.
/// Persisted as `LightPalette`.
#[derive(Debug, Default, PartialEq, Eq)]
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
}

/// The palette used while the resolved mode is dark.
/// Persisted as `DarkPalette`.
#[derive(Debug, Default, PartialEq, Eq)]
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
}

/// All persisted appearance settings, injected into the shell before
/// first paint (see `shell_html` in the view).
#[derive(Debug, Default)]
pub struct Settings {
    pub theme: Theme,
    pub light: LightPalette,
    pub dark: DarkPalette,
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
