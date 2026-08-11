/// Version baked in at compile time by `build.rs`, from `git describe --tags`.
///
/// A clean tagged build reads `0.1.0`; commits past the tag read
/// `0.1.0-3-gab12cd3`; a repo with no tags degrades to a bare commit hash.
const BUILT_VERSION: &str = env!("SYNCPOINT_VERSION");

/// The version to show the user.
///
/// `tauri::is_dev()` is `!cfg!(feature = "custom-protocol")` — the feature
/// `tauri build` turns on and `tauri dev` does not — so this marks exactly the
/// builds launched through `pnpm tauri dev`.
pub fn describe() -> String {
    with_dev_suffix(BUILT_VERSION, tauri::is_dev())
}

fn with_dev_suffix(base: &str, is_dev: bool) -> String {
    if is_dev {
        format!("{base}-dev")
    } else {
        base.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_builds_show_the_bare_version() {
        assert_eq!(with_dev_suffix("0.1.0", false), "0.1.0");
    }

    #[test]
    fn dev_builds_are_marked() {
        assert_eq!(with_dev_suffix("0.1.0", true), "0.1.0-dev");
        // A version already carrying git's commits-since-tag detail keeps it.
        assert_eq!(
            with_dev_suffix("0.1.0-3-gab12cd3", true),
            "0.1.0-3-gab12cd3-dev"
        );
    }

    /// Exercises the real build configuration rather than the helper: run
    /// `cargo test` for the dev branch and
    /// `cargo test --features tauri/custom-protocol` for the release branch.
    #[test]
    fn the_dev_marker_tracks_the_actual_build_kind() {
        let described = describe();
        eprintln!("is_dev={} version={described}", tauri::is_dev());
        assert!(described.starts_with(BUILT_VERSION));
        if tauri::is_dev() {
            assert!(described.ends_with("-dev"), "dev build unmarked: {described}");
        } else {
            assert!(
                !described.ends_with("-dev"),
                "release build wrongly marked: {described}"
            );
        }
    }

    #[test]
    fn the_baked_in_version_is_present_and_unprefixed() {
        assert!(!BUILT_VERSION.is_empty());
        // build.rs strips the `v` from tags like `v0.1.0`.
        assert!(!BUILT_VERSION.starts_with('v'));
    }
}
