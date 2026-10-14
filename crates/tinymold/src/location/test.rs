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
