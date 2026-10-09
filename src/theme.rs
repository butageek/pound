//! Theme selection (Auto follows the system; Light/Dark are forced).

/// The user's theme choice. Persisted as a string under
/// `HKCU\Software\Pound\Theme` (see `register::load_theme`/`save_theme`)
/// and injected into the shell before first paint.
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

    /// Only the three known values parse — garbage from the registry (or
    /// a stale future version) falls back to Auto.
    pub fn parse(value: &str) -> Option<Theme> {
        match value.trim() {
            "auto" => Some(Theme::Auto),
            "light" => Some(Theme::Light),
            "dark" => Some(Theme::Dark),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn themes_round_trip_through_strings() {
        for theme in [Theme::Auto, Theme::Light, Theme::Dark] {
            assert_eq!(Theme::parse(theme.as_str()), Some(theme));
        }
    }

    #[test]
    fn unknown_values_do_not_parse() {
        assert_eq!(Theme::parse("solarized"), None);
        assert_eq!(Theme::parse(""), None);
        assert_eq!(Theme::default(), Theme::Auto);
    }
}
