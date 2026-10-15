//! Unit tests for default store resolution.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;

use super::*;

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let map: HashMap<String, OsString> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
        .collect();
    move |name| map.get(name).cloned()
}

#[test]
fn an_explicit_store_wins() {
    let lookup = env(&[("TINYMOLD_STORE", "/fast/disk"), ("HOME", "/home/u")]);
    assert_eq!(default_store(lookup), Some(PathBuf::from("/fast/disk")));
}

#[test]
fn xdg_cache_home_beats_home() {
    let lookup = env(&[("XDG_CACHE_HOME", "/xdg"), ("HOME", "/home/u")]);
    assert_eq!(default_store(lookup), Some(PathBuf::from("/xdg/tinymold")));
}

#[test]
fn local_app_data_is_used_on_windows_style_environments() {
    let lookup = env(&[("LOCALAPPDATA", "C:/Users/u/AppData/Local")]);
    assert_eq!(
        default_store(lookup),
        Some(PathBuf::from("C:/Users/u/AppData/Local").join("tinymold"))
    );
}

#[test]
fn empty_variables_are_ignored() {
    let lookup = env(&[("TINYMOLD_STORE", ""), ("HOME", "/home/u")]);
    assert_eq!(
        default_store(lookup),
        Some(PathBuf::from("/home/u/.cache/tinymold"))
    );
}

#[test]
fn nothing_set_means_no_location() {
    assert_eq!(default_store(env(&[])), None);
}

#[test]
fn the_server_dir_prefers_its_own_variable_and_never_the_wrapper_store() {
    let env = |vars: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    };

    assert_eq!(
        default_server_dir(env(&[("TINYMOLD_SERVER_DIR", "/srv/cache"), ("HOME", "/h")])),
        Some(PathBuf::from("/srv/cache"))
    );
    assert_eq!(
        default_server_dir(env(&[("XDG_CACHE_HOME", "/c"), ("TINYMOLD_STORE", "/c/tinymold")])),
        Some(PathBuf::from("/c/tinymold-server"))
    );
    assert_eq!(
        default_server_dir(env(&[("HOME", "/h"), ("TINYMOLD_SERVER_DIR", "")])),
        Some(PathBuf::from("/h/.cache/tinymold-server"))
    );
    assert_eq!(default_server_dir(env(&[])), None);
}
