use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follow the OS appearance.
    #[default]
    System,
    Light,
    Dark,
    /// Catppuccin. Latte is the light flavour; the other three are dark, which
    /// the frontend needs to know since native chrome only has two modes.
    #[serde(rename = "catppuccin-latte")]
    CatppuccinLatte,
    #[serde(rename = "catppuccin-frappe")]
    CatppuccinFrappe,
    #[serde(rename = "catppuccin-macchiato")]
    CatppuccinMacchiato,
    #[serde(rename = "catppuccin-mocha")]
    CatppuccinMocha,
}

/// Seeded into every new profile. Kept here rather than in the frontend so the
/// list is stored, editable, and survives a rebuild.
fn default_excludes() -> Vec<String> {
    vec![".git".into(), ".DS_Store".into(), "node_modules".into()]
}

fn default_debounce_ms() -> u64 {
    500
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub theme: Theme,
    /// Mirrors the OS launch-at-login registration. The registration itself is
    /// the source of truth; this is what gets restored if it goes missing.
    #[serde(default)]
    pub open_at_startup: bool,
    /// Applied when a profile is created. Editing this deliberately does not
    /// touch existing profiles — they keep whatever they were saved with.
    #[serde(default = "default_excludes")]
    pub default_excludes: Vec<String>,
    /// Prefilled into a new profile's remote path, e.g. `/srv/apps`. Empty
    /// means no prefill. Stored without a trailing slash.
    #[serde(default)]
    pub default_remote_prefix: String,
    /// Where the folder picker opens when choosing a profile's source. Not
    /// prefilled into the field: a partial local path would fail validation
    /// immediately, since the source must be a directory that exists.
    #[serde(default)]
    pub default_local_root: String,
    #[serde(default = "default_debounce_ms")]
    pub default_debounce_ms: u64,
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        let prefix = self.default_remote_prefix.trim();
        if !prefix.is_empty() && !prefix.starts_with('/') && !prefix.starts_with("~/") {
            return Err("Remote path prefix must start with / or ~/.".into());
        }
        let local = self.default_local_root.trim();
        if !local.is_empty() && !local.starts_with('/') {
            return Err("Local folder must be an absolute path.".into());
        }
        if !(50..=60_000).contains(&self.default_debounce_ms) {
            return Err("Debounce must be between 50 and 60000 ms.".into());
        }
        Ok(())
    }

    /// Store one canonical form so the frontend can append a separator without
    /// having to guess whether the user typed one.
    pub fn normalize(&mut self) {
        let prefix = self.default_remote_prefix.trim().trim_end_matches('/');
        self.default_remote_prefix = prefix.to_string();
        let local = self.default_local_root.trim().trim_end_matches('/');
        self.default_local_root = local.to_string();
        for pattern in &mut self.default_excludes {
            *pattern = pattern.trim().to_string();
        }
        self.default_excludes.retain(|p| !p.is_empty());
    }
}

// Hand-written rather than derived: a derived Default would give an empty
// exclude list, which disagrees with the serde default and would leave new
// installs syncing .git and node_modules.
impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            open_at_startup: false,
            default_excludes: default_excludes(),
            default_remote_prefix: String::new(),
            default_local_root: String::new(),
            default_debounce_ms: default_debounce_ms(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_system_theme_and_no_autostart() {
        let settings = Settings::default();
        assert_eq!(settings.theme, Theme::System);
        assert!(!settings.open_at_startup);
    }

    #[test]
    fn the_derived_and_serde_defaults_agree() {
        // These are two separate code paths; if they drift, a fresh install and
        // a config missing the key would seed different excludes.
        assert_eq!(
            Settings::default().default_excludes,
            serde_json::from_str::<Settings>("{}").unwrap().default_excludes
        );
        assert!(Settings::default()
            .default_excludes
            .contains(&".git".to_string()));
    }

    #[test]
    fn normalize_strips_trailing_slashes_from_the_prefix() {
        for input in ["/srv/apps/", "/srv/apps///", "  /srv/apps  "] {
            let mut settings = Settings {
                default_remote_prefix: input.into(),
                ..Settings::default()
            };
            settings.normalize();
            assert_eq!(settings.default_remote_prefix, "/srv/apps", "from {input:?}");
        }
    }

    #[test]
    fn normalize_tidies_the_exclude_list() {
        let mut settings = Settings {
            default_excludes: vec!["  .git ".into(), "   ".into(), "node_modules".into()],
            ..Settings::default()
        };
        settings.normalize();
        assert_eq!(settings.default_excludes, vec![".git", "node_modules"]);
    }

    #[test]
    fn validate_requires_an_absolute_or_home_relative_prefix() {
        let ok = |p: &str| Settings { default_remote_prefix: p.into(), ..Settings::default() }.validate();
        assert!(ok("").is_ok(), "empty means no prefill");
        assert!(ok("/srv/apps").is_ok());
        assert!(ok("~/apps").is_ok());
        assert!(ok("srv/apps").is_err());
        assert!(ok("../apps").is_err());
    }

    #[test]
    fn an_explicitly_empty_exclude_list_is_respected() {
        // Clearing the list must stick, not silently fall back to the defaults.
        let settings: Settings = serde_json::from_str(r#"{"defaultExcludes":[]}"#).unwrap();
        assert!(settings.default_excludes.is_empty());
    }

    #[test]
    fn validate_bounds_the_default_debounce() {
        let at = |ms: u64| Settings { default_debounce_ms: ms, ..Settings::default() }.validate();
        assert!(at(500).is_ok());
        assert!(at(50).is_ok());
        assert!(at(60_000).is_ok());
        assert!(at(49).is_err());
        assert!(at(60_001).is_err());
    }

    #[test]
    fn validate_requires_an_absolute_local_root() {
        let at = |p: &str| Settings { default_local_root: p.into(), ..Settings::default() }.validate();
        assert!(at("").is_ok(), "empty means the picker uses its own default");
        assert!(at("/Users/me/dev").is_ok());
        assert!(at("dev").is_err());
        // `~` is not expanded by the folder picker, so it would be a dead path.
        assert!(at("~/dev").is_err());
    }

    #[test]
    fn normalize_strips_trailing_slashes_from_the_local_root() {
        let mut settings = Settings {
            default_local_root: "  /Users/me/dev//  ".into(),
            ..Settings::default()
        };
        settings.normalize();
        assert_eq!(settings.default_local_root, "/Users/me/dev");
    }

    #[test]
    fn theme_round_trips_as_a_lowercase_string() {
        for (theme, expected) in [
            (Theme::System, "\"system\""),
            (Theme::Light, "\"light\""),
            (Theme::Dark, "\"dark\""),
            (Theme::CatppuccinLatte, "\"catppuccin-latte\""),
            (Theme::CatppuccinFrappe, "\"catppuccin-frappe\""),
            (Theme::CatppuccinMacchiato, "\"catppuccin-macchiato\""),
            (Theme::CatppuccinMocha, "\"catppuccin-mocha\""),
        ] {
            let json = serde_json::to_string(&theme).unwrap();
            assert_eq!(json, expected);
            assert_eq!(serde_json::from_str::<Theme>(&json).unwrap(), theme);
        }
    }

    /// A variant with no entry in the theme map falls through to the default
    /// palette and looks broken for no visible reason, so the two are pinned
    /// together. The map is the single place themes are defined, which is why
    /// this reads the stylesheet source rather than compiled CSS.
    #[test]
    fn every_explicit_theme_has_a_palette() {
        let scss = std::fs::read_to_string("../src/styles/_themes.scss")
            .expect("theme stylesheet beside the crate");
        for theme in [
            Theme::Light,
            Theme::Dark,
            Theme::CatppuccinLatte,
            Theme::CatppuccinFrappe,
            Theme::CatppuccinMacchiato,
            Theme::CatppuccinMocha,
        ] {
            let serialized = serde_json::to_string(&theme).unwrap();
            let key = format!("{}:", serialized); // e.g. "catppuccin-mocha":
            assert!(scss.contains(&key), "no palette for theme {serialized}");
        }
        // System is the absence of a choice, not a palette: the stylesheet keys
        // "follow the OS" on data-theme being missing entirely.
        assert!(!scss.contains("\"system\":"));
    }

    #[test]
    fn a_config_written_before_settings_existed_still_loads() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings, Settings::default());
    }
}
