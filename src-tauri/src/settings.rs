use serde::{Deserialize, Serialize};

use crate::template;

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
    Dracula,
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
    /// Prefilled into a new profile's source path, e.g. `~/dev/{slug}`. Empty
    /// means no prefill. Stored without a trailing slash. See `template`.
    #[serde(default)]
    pub source_template: String,
    /// The same for its remote path, e.g. `/home/code/dev/{slug}`.
    #[serde(default)]
    pub target_template: String,
    /// Written by versions before templates. Read once by
    /// [`Settings::adopt_legacy_prefix`], and never written back.
    #[serde(default, rename = "defaultRemotePrefix", skip_serializing)]
    pub legacy_remote_prefix: Option<String>,
    /// Where the folder picker opens when choosing a profile's source, and what
    /// `{source_rel}` is measured from.
    #[serde(default)]
    pub default_local_root: String,
    #[serde(default = "default_debounce_ms")]
    pub default_debounce_ms: u64,
    /// Hide the detail pane and narrow the main window to the profile list.
    /// Saved, so the app reopens the way it was left.
    #[serde(default)]
    pub compact: bool,
    /// Global suspend. Deliberately separate from each profile's `enabled`
    /// flag, so resuming restores exactly what was running before rather than
    /// switching on profiles that were deliberately left off.
    #[serde(default)]
    pub paused: bool,
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        template::check_source(&self.source_template)?;
        template::check_target(&self.target_template)?;
        let local = self.default_local_root.trim();
        if !local.is_empty() && !local.starts_with('/') {
            return Err("Local folder must be an absolute path.".into());
        }
        if !(50..=60_000).contains(&self.default_debounce_ms) {
            return Err("Debounce must be between 50 and 60000 ms.".into());
        }
        Ok(())
    }

    /// Store one canonical form, however the user typed it.
    pub fn normalize(&mut self) {
        self.source_template = self.source_template.trim().trim_end_matches('/').to_string();
        self.target_template = self.target_template.trim().trim_end_matches('/').to_string();
        let local = self.default_local_root.trim().trim_end_matches('/');
        self.default_local_root = local.to_string();
        for pattern in &mut self.default_excludes {
            *pattern = pattern.trim().to_string();
        }
        self.default_excludes.retain(|p| !p.is_empty());
    }

    /// Carry a saved remote path prefix over to a template.
    pub fn adopt_legacy_prefix(&mut self) {
        if let Some(prefix) = self.legacy_remote_prefix.take() {
            if self.target_template.is_empty() {
                self.target_template = template_from_prefix(&prefix);
            }
        }
    }
}

/// `/srv/apps` becomes `/srv/apps/{slug}`. The prefill used to stop at the prefix
/// for the user to finish, and left as it was, forgetting to would send every
/// profile to the same folder, wiping each other's files with delete on.
pub fn template_from_prefix(prefix: &str) -> String {
    let prefix = prefix.trim().trim_end_matches('/');
    if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/{{slug}}")
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
            source_template: String::new(),
            target_template: String::new(),
            legacy_remote_prefix: None,
            default_local_root: String::new(),
            default_debounce_ms: default_debounce_ms(),
            compact: false,
            paused: false,
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
        assert!(!settings.paused, "a fresh install must not start suspended");
    }

    #[test]
    fn pausing_does_not_disturb_the_enabled_flags() {
        // Pause is a separate switch precisely so resuming can restore what was
        // running; a config written before it existed reads as not paused.
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(!settings.paused);
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
    fn normalize_strips_trailing_slashes_from_the_templates() {
        for input in ["/srv/apps/", "/srv/apps///", "  /srv/apps  "] {
            let mut settings = Settings {
                source_template: input.into(),
                target_template: input.into(),
                ..Settings::default()
            };
            settings.normalize();
            assert_eq!(settings.source_template, "/srv/apps", "from {input:?}");
            assert_eq!(settings.target_template, "/srv/apps", "from {input:?}");
        }
    }

    #[test]
    fn a_saved_prefix_becomes_a_template_and_is_not_written_back() {
        let mut settings: Settings =
            serde_json::from_str(r#"{"defaultRemotePrefix":"/srv/apps"}"#).unwrap();

        settings.adopt_legacy_prefix();

        assert_eq!(settings.target_template, "/srv/apps/{slug}");
        assert_eq!(settings.legacy_remote_prefix, None);
        let written = serde_json::to_string(&settings).unwrap();
        assert!(!written.contains("defaultRemotePrefix"), "{written}");
    }

    #[test]
    fn an_empty_saved_prefix_stays_no_prefill() {
        let mut settings: Settings =
            serde_json::from_str(r#"{"defaultRemotePrefix":""}"#).unwrap();

        settings.adopt_legacy_prefix();

        assert_eq!(settings.target_template, "");
    }

    #[test]
    fn a_template_already_set_is_not_replaced_by_an_old_prefix() {
        let mut settings: Settings = serde_json::from_str(
            r#"{"defaultRemotePrefix":"/old","targetTemplate":"/new/{name}"}"#,
        )
        .unwrap();

        settings.adopt_legacy_prefix();

        assert_eq!(settings.target_template, "/new/{name}");
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
    fn validate_runs_the_template_checks_on_both() {
        let source = |t: &str| Settings { source_template: t.into(), ..Settings::default() }.validate();
        assert!(source("~/dev/{slug}").is_ok());
        assert!(source("/x/{source_name}").is_err());

        let ok = |p: &str| Settings { target_template: p.into(), ..Settings::default() }.validate();
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
            (Theme::Dracula, "\"dracula\""),
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
            Theme::Dracula,
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
