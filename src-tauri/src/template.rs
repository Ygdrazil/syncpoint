//! What a new profile's paths are prefilled with, from the templates in
//! `Settings`: `/home/code/dev/{slug}`.
//!
//! A template only fills a field while a profile is being created. What gets
//! saved is plain text, so changing a template later can never move an existing
//! profile's destination, which with delete on could wipe the wrong folder.
//!
//! A value with nothing to give stays in the text as `{slug}`. Dropping it would
//! be the dangerous choice: `/home/code/dev/{slug}` with no slug would become
//! `/home/code/dev/`, and syncing there with delete on wipes every project in
//! it. `Profile::validate` refuses a path still holding one.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use serde::Serialize;

use crate::connection::Connection;
use crate::folder::Folder;
use crate::profile::Profile;
use crate::settings::Settings;

const VALUES: &[&str] = &[
    "name",
    "slug",
    "folder",
    "folder_path",
    "connection",
    "user",
    "host",
    "source_name",
    "source_parent",
    "source_rel",
    "home",
    "machine",
];

/// These come from the source path, so a source template cannot be built on them.
const FROM_THE_SOURCE: &[&str] = &["source_name", "source_parent", "source_rel"];

type Values = HashMap<&'static str, String>;

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Rendered {
    pub source: String,
    pub target: String,
}

/// The parts that come from this Mac rather than from the profile.
struct ThisMac {
    home: Option<String>,
    name: Option<String>,
}

fn this_mac() -> ThisMac {
    static NAME: OnceLock<Option<String>> = OnceLock::new();
    let name = NAME.get_or_init(|| {
        let output = Command::new("/bin/hostname").output().ok()?;
        let hostname = String::from_utf8_lossy(&output.stdout);
        let name = slugify(hostname.trim().trim_end_matches(".local"));
        (!name.is_empty()).then_some(name)
    });
    ThisMac {
        home: std::env::var("HOME").ok().filter(|home| !home.is_empty()),
        name: name.clone(),
    }
}

/// Lowercase words joined by dashes. Keeps letters outside ASCII, so a name in
/// another alphabet still has a slug. Nothing in the result can be read as a
/// path separator or a `..`.
fn slugify(text: &str) -> String {
    let lowered = text.to_lowercase();
    lowered
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Slugs of a folder and its parents, outermost first. Empty when the profile is
/// in no folder, or when one of the names has no slug to offer.
fn folder_slugs(folders: &[Folder], folder_id: Option<&str>) -> Vec<String> {
    let mut slugs = Vec::new();
    let mut current = folder_id;
    // The bound is for a hand-edited config whose parents form a loop.
    for _ in 0..folders.len() {
        let Some(folder) = current.and_then(|id| folders.iter().find(|f| f.id == id)) else {
            break;
        };
        slugs.push(slugify(&folder.name));
        current = folder.parent_id.as_deref();
    }
    if slugs.iter().any(String::is_empty) {
        return Vec::new();
    }
    slugs.reverse();
    slugs
}

fn values(
    settings: &Settings,
    profile: &Profile,
    connection: Option<&Connection>,
    folders: &[Folder],
    mac: &ThisMac,
) -> Values {
    let mut values = Values::new();
    let mut put = |value: &'static str, text: String| {
        if !text.is_empty() {
            values.insert(value, text);
        }
    };

    // A slash in a name is not a request for another directory level.
    put("name", profile.name.trim().replace('/', "-"));
    put("slug", slugify(&profile.name));

    let slugs = folder_slugs(folders, profile.folder_id.as_deref());
    put("folder", slugs.last().cloned().unwrap_or_default());
    put("folder_path", slugs.join("/"));

    if let Some(connection) = connection {
        put("connection", slugify(&connection.name));
        put("user", connection.user.trim().to_string());
        put("host", connection.host.trim().to_string());
    }

    let source = Path::new(profile.source.trim());
    if source.is_absolute() {
        let last_part = |path: &Path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        put("source_name", last_part(source));
        put("source_parent", source.parent().map(last_part).unwrap_or_default());

        let root = settings.default_local_root.trim();
        if !root.is_empty() {
            if let Ok(relative) = source.strip_prefix(root) {
                put("source_rel", relative.to_string_lossy().into_owned());
            }
        }
    }

    put("home", mac.home.clone().unwrap_or_default());
    put("machine", mac.name.clone().unwrap_or_default());
    values
}

/// Every `{...}` in the text, without its braces.
fn placeholders(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            break;
        };
        found.push(&after[..close]);
        rest = &after[close + 1..];
    }
    found
}

/// One pass over the template, so a value that happens to contain `{slug}` is
/// not expanded again.
fn fill(template: &str, values: &Values) -> String {
    let mut filled = String::new();
    let mut rest = template.trim();
    while let Some(open) = rest.find('{') {
        filled.push_str(&rest[..open]);
        let from_brace = &rest[open..];
        let Some(close) = from_brace.find('}') else {
            rest = from_brace;
            break;
        };
        match values.get(&from_brace[1..close]) {
            Some(value) => filled.push_str(value),
            None => filled.push_str(&from_brace[..=close]),
        }
        rest = &from_brace[close + 1..];
    }
    filled.push_str(rest);
    filled
}

fn fill_paths(
    settings: &Settings,
    profile: &Profile,
    connection: Option<&Connection>,
    folders: &[Folder],
    mac: &ThisMac,
) -> Rendered {
    let values = values(settings, profile, connection, folders, mac);

    let mut for_source = values.clone();
    for value in FROM_THE_SOURCE {
        for_source.remove(value);
    }
    let source = fill(&settings.source_template, &for_source);
    // A source has to be absolute, and `~` means this Mac here. In a target it
    // is left alone, because there it means the remote user's home.
    let source = match (source.strip_prefix("~/"), &mac.home) {
        (Some(rest), Some(home)) => format!("{home}/{rest}"),
        _ => source,
    };

    Rendered {
        source,
        target: fill(&settings.target_template, &values),
    }
}

/// A new profile's paths as they stand, from whatever its form holds so far.
pub fn render(
    settings: &Settings,
    profile: &Profile,
    connection: Option<&Connection>,
    folders: &[Folder],
) -> Rendered {
    fill_paths(settings, profile, connection, folders, &this_mac())
}

/// What these templates give for a made-up profile, for the settings page.
pub fn preview(source_template: &str, target_template: &str, local_root: &str) -> Rendered {
    let mac = this_mac();
    let root = local_root.trim().trim_end_matches('/');
    let base = if root.is_empty() {
        format!("{}/dev", mac.home.as_deref().unwrap_or_default())
    } else {
        root.to_string()
    };
    let settings = Settings {
        source_template: source_template.to_string(),
        target_template: target_template.to_string(),
        default_local_root: root.to_string(),
        ..Settings::default()
    };
    let folder = |id: &str, name: &str, parent_id: Option<&str>| Folder {
        id: id.into(),
        name: name.into(),
        parent_id: parent_id.map(String::from),
        position: 0,
    };
    let profile = Profile {
        id: String::new(),
        name: "My Project".into(),
        source: format!("{base}/clients/my-project"),
        connection_id: "c".into(),
        remote_path: String::new(),
        enabled: true,
        delete: true,
        excludes: Vec::new(),
        debounce_ms: 500,
        folder_id: Some("acme".into()),
        position: 0,
    };
    let connection = Connection {
        id: "c".into(),
        name: "My Server".into(),
        user: "username".into(),
        host: "example.com".into(),
        port: 22,
        identity_file: String::new(),
    };
    let folders = [
        folder("clients", "Clients", None),
        folder("acme", "Acme", Some("clients")),
    ];
    fill_paths(&settings, &profile, Some(&connection), &folders, &mac)
}

fn check(label: &str, template: &str, not_here: &[&str]) -> Result<(), String> {
    let template = template.trim();
    if template.is_empty() {
        return Ok(());
    }
    for placeholder in placeholders(template) {
        if not_here.contains(&placeholder) {
            return Err(format!(
                "{label} cannot use {{{placeholder}}}: that value comes from the source path."
            ));
        }
        if !VALUES.contains(&placeholder) {
            return Err(format!("{label}: {{{placeholder}}} is not a value it can use."));
        }
    }
    if !["/", "~/", "{home}"].iter().any(|start| template.starts_with(start)) {
        return Err(format!("{label} must start with /, ~/ or {{home}}."));
    }
    if template.split('/').any(|segment| segment == "." || segment == "..") {
        return Err(format!("{label} cannot contain . or .. segments."));
    }
    Ok(())
}

pub fn check_source(template: &str) -> Result<(), String> {
    check("Source path template", template, FROM_THE_SOURCE)
}

pub fn check_target(template: &str) -> Result<(), String> {
    check("Remote path template", template, &[])
}

/// The first value this path is still waiting for, if any.
pub fn unfilled(path: &str) -> Option<&str> {
    placeholders(path).into_iter().find(|p| VALUES.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac() -> ThisMac {
        ThisMac {
            home: Some("/Users/me".into()),
            name: Some("my-mac".into()),
        }
    }

    fn connection() -> Connection {
        Connection {
            id: "c".into(),
            name: "Dev box".into(),
            user: "code".into(),
            host: "dev".into(),
            port: 22,
            identity_file: String::new(),
        }
    }

    fn folders() -> Vec<Folder> {
        let folder = |id: &str, name: &str, parent_id: Option<&str>| Folder {
            id: id.into(),
            name: name.into(),
            parent_id: parent_id.map(String::from),
            position: 0,
        };
        vec![
            folder("clients", "Clients", None),
            folder("acme", "Acme", Some("clients")),
        ]
    }

    fn profile() -> Profile {
        Profile {
            id: String::new(),
            name: "Site API Domain".into(),
            source: "/Users/me/dev/clients/site-api-domain".into(),
            connection_id: "c".into(),
            remote_path: String::new(),
            enabled: true,
            delete: true,
            excludes: Vec::new(),
            debounce_ms: 500,
            folder_id: Some("acme".into()),
            position: 0,
        }
    }

    fn target(template: &str, profile: &Profile) -> String {
        let settings = Settings {
            target_template: template.into(),
            default_local_root: "/Users/me/dev".into(),
            ..Settings::default()
        };
        fill_paths(&settings, profile, Some(&connection()), &folders(), &mac()).target
    }

    fn source(template: &str, profile: &Profile) -> String {
        let settings = Settings {
            source_template: template.into(),
            ..Settings::default()
        };
        fill_paths(&settings, profile, Some(&connection()), &folders(), &mac()).source
    }

    #[test]
    fn every_value_comes_from_where_it_should() {
        let profile = profile();
        for (template, expected) in [
            ("/x/{name}", "/x/Site API Domain"),
            ("/x/{slug}", "/x/site-api-domain"),
            ("/x/{folder}", "/x/acme"),
            ("/x/{folder_path}", "/x/clients/acme"),
            ("/x/{connection}", "/x/dev-box"),
            ("/x/{user}", "/x/code"),
            ("/x/{host}", "/x/dev"),
            ("/x/{source_name}", "/x/site-api-domain"),
            ("/x/{source_parent}", "/x/clients"),
            ("/x/{source_rel}", "/x/clients/site-api-domain"),
            ("{home}/x", "/Users/me/x"),
            ("/x/{machine}", "/x/my-mac"),
        ] {
            assert_eq!(target(template, &profile), expected, "for {template}");
        }
    }

    #[test]
    fn the_name_can_sit_anywhere_in_the_path() {
        let profile = profile();
        assert_eq!(target("/srv/{slug}", &profile), "/srv/site-api-domain");
        assert_eq!(target("/srv/{slug}/current", &profile), "/srv/site-api-domain/current");
        assert_eq!(target("/srv/app-{slug}-v2", &profile), "/srv/app-site-api-domain-v2");
    }

    #[test]
    fn every_value_the_app_names_is_one_this_module_can_fill() {
        // A value added to the list but not to values() would pass the settings
        // check and then never fill.
        let settings = Settings {
            default_local_root: "/Users/me/dev".into(),
            ..Settings::default()
        };
        let filled = values(&settings, &profile(), Some(&connection()), &folders(), &mac());
        for value in VALUES {
            assert!(filled.contains_key(value), "{value} has no value");
        }
    }

    #[test]
    fn a_slug_cannot_climb_out_of_its_folder() {
        assert_eq!(slugify("Site API Domain"), "site-api-domain");
        assert_eq!(slugify("  Été 2026! "), "été-2026");
        assert_eq!(slugify("my_app"), "my_app");
        assert_eq!(slugify("a/b"), "a-b");
        assert_eq!(slugify("../../etc"), "etc");
        assert_eq!(slugify(".."), "");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn a_value_with_nothing_to_give_stays_in_the_path() {
        // Dropping it would leave /home/code/dev/, the parent of every project,
        // as the destination of a sync that deletes.
        let mut unnamed = profile();
        unnamed.name = String::new();
        assert_eq!(target("/home/code/dev/{slug}", &unnamed), "/home/code/dev/{slug}");

        let mut symbols = profile();
        symbols.name = "!!!".into();
        assert_eq!(target("/home/code/dev/{slug}", &symbols), "/home/code/dev/{slug}");

        let mut top_level = profile();
        top_level.folder_id = None;
        assert_eq!(target("/srv/{folder}/x", &top_level), "/srv/{folder}/x");
        assert_eq!(target("/srv/{folder_path}/x", &top_level), "/srv/{folder_path}/x");
    }

    #[test]
    fn a_connection_with_no_user_leaves_user_unfilled() {
        let mut no_user = connection();
        no_user.user = "  ".into();
        let settings = Settings {
            target_template: "/home/{user}/x".into(),
            ..Settings::default()
        };

        let rendered = fill_paths(&settings, &profile(), Some(&no_user), &folders(), &mac());

        assert_eq!(rendered.target, "/home/{user}/x");
    }

    #[test]
    fn no_connection_yet_leaves_its_values_unfilled() {
        let settings = Settings {
            target_template: "/{connection}/{user}/{host}".into(),
            ..Settings::default()
        };

        let rendered = fill_paths(&settings, &profile(), None, &folders(), &mac());

        assert_eq!(rendered.target, "/{connection}/{user}/{host}");
    }

    #[test]
    fn source_rel_only_fills_inside_the_local_root() {
        let mut elsewhere = profile();
        elsewhere.source = "/Volumes/ext/site".into();
        assert_eq!(target("/x/{source_rel}", &elsewhere), "/x/{source_rel}");

        // At the root itself there is nothing left to be relative to.
        let mut at_root = profile();
        at_root.source = "/Users/me/dev".into();
        assert_eq!(target("/x/{source_rel}", &at_root), "/x/{source_rel}");
    }

    #[test]
    fn no_source_chosen_yet_leaves_its_values_unfilled() {
        let mut no_source = profile();
        no_source.source = String::new();
        for template in ["/x/{source_name}", "/x/{source_parent}", "/x/{source_rel}"] {
            assert_eq!(target(template, &no_source), template);
        }
    }

    #[test]
    fn tilde_means_this_mac_in_a_source_and_the_remote_in_a_target() {
        let profile = profile();
        assert_eq!(source("~/dev/{slug}", &profile), "/Users/me/dev/site-api-domain");
        assert_eq!(target("~/dev/{slug}", &profile), "~/dev/site-api-domain");
    }

    #[test]
    fn a_source_template_cannot_be_built_on_the_source() {
        assert_eq!(source("/x/{source_name}", &profile()), "/x/{source_name}");
        assert!(check_source("/x/{source_name}").is_err());
        assert!(check_target("/x/{source_name}").is_ok());
    }

    #[test]
    fn a_value_is_not_expanded_a_second_time() {
        let mut odd = profile();
        odd.name = "{slug}".into();
        // Left looking unfinished, which is what stops it being saved.
        assert_eq!(target("/x/{name}", &odd), "/x/{slug}");
    }

    #[test]
    fn a_slash_in_a_name_is_not_another_directory() {
        let mut slashed = profile();
        slashed.name = "a/b".into();
        assert_eq!(target("/x/{name}", &slashed), "/x/a-b");
    }

    #[test]
    fn a_loop_of_folders_cannot_hang_the_form() {
        let looped = vec![
            Folder { id: "a".into(), name: "A".into(), parent_id: Some("b".into()), position: 0 },
            Folder { id: "b".into(), name: "B".into(), parent_id: Some("a".into()), position: 0 },
        ];
        assert_eq!(folder_slugs(&looped, Some("a")).len(), 2);
    }

    #[test]
    fn the_settings_check_catches_what_could_never_work() {
        assert!(check_target("").is_ok(), "empty means no prefill");
        assert!(check_target("/srv/{slug}").is_ok());
        assert!(check_target("~/dev/{slug}").is_ok());
        assert!(check_target("{home}/x").is_ok());

        assert!(check_target("srv/{slug}").is_err(), "relative");
        assert!(check_target("{slug}/x").is_err(), "relative once filled");
        assert!(check_target("/srv/{slgu}").is_err(), "a typo");
        assert!(check_target("/srv/{Slug}").is_err(), "values are lowercase");
        assert!(check_target("/srv/../{slug}").is_err());
        assert!(check_target("/srv/./{slug}").is_err());
    }

    #[test]
    fn only_real_values_count_as_unfilled() {
        assert_eq!(unfilled("/x/{slug}/y"), Some("slug"));
        assert_eq!(unfilled("/x/{other}/{host}"), Some("host"));
        assert_eq!(unfilled("/x/{other}"), None, "a folder can have braces in its name");
        assert_eq!(unfilled("/x/plain"), None);
        assert_eq!(unfilled("/x/{slug"), None);
    }

    #[test]
    fn the_settings_example_goes_through_the_real_rendering() {
        let rendered = preview("/a/{folder_path}/{slug}", "/b/{source_rel}", "/Users/me/dev/");

        assert_eq!(rendered.source, "/a/clients/acme/my-project");
        assert_eq!(rendered.target, "/b/clients/my-project");
    }

    #[test]
    fn an_empty_template_prefills_nothing() {
        let rendered = preview("", "", "");

        assert_eq!(rendered.source, "");
        assert_eq!(rendered.target, "");
    }
}
