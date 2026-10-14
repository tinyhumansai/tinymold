//! Unit tests for the test key: its pinned encoding, what moves it, and
//! what leaves it alone.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use super::*;

fn roots() -> Roots {
    Roots::new(
        Path::new("/w/a/crates/demo"),
        Path::new("/w/a/target/debug/deps"),
        Some(Path::new("/home/me/.cargo")),
    )
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

struct Case {
    binary: Digest,
    via: Option<String>,
    args: Vec<String>,
    host: String,
    package_tree: Digest,
    declared_inputs: Digest,
    env: Vec<(String, String)>,
}

impl Case {
    fn new() -> Self {
        Self {
            binary: Digest::of_bytes(b"test binary"),
            via: None,
            args: strings(&["--quiet", "parser"]),
            host: "macos aarch64 25.5.0".to_owned(),
            package_tree: tree_digest(&[("src/lib.rs".to_owned(), Digest::of_bytes(b"lib"))]),
            declared_inputs: tree_digest(&[]),
            env: env(&[
                ("CARGO_MANIFEST_DIR", "/w/a/crates/demo"),
                ("PATH", "/usr/bin"),
            ]),
        }
    }

    fn key(&self) -> Digest {
        let roots = roots();
        test_key(&TestKeyInputs {
            binary: &self.binary,
            via: self.via.as_deref(),
            args: &self.args,
            roots: &roots,
            host: &self.host,
            package_tree: &self.package_tree,
            declared_inputs: &self.declared_inputs,
            env: &self.env,
        })
    }
}

#[test]
fn the_test_key_encoding_is_pinned() {
    // Changing the encoding orphans every recorded test result, so this
    // value only changes together with the schema tag.
    assert_eq!(Case::new().key().to_hex(), PINNED_TEST_KEY);
    assert_eq!(
        tree_digest(&[("a".to_owned(), Digest::of_bytes(b"a"))]).to_hex(),
        PINNED_TREE
    );
}

const PINNED_TEST_KEY: &str = "";
const PINNED_TREE: &str = "";

#[test]
fn every_keyed_field_moves_the_key() {
    let reference = Case::new().key();
    let changes: Vec<(&str, fn(&mut Case))> = vec![
        ("binary", |case| case.binary = Digest::of_bytes(b"other")),
        ("via", |case| {
            case.via = Some("qemu-aarch64 0123".to_owned())
        }),
        ("args", |case| case.args.push("--exact".to_owned())),
        ("arg order", |case| case.args.reverse()),
        ("host", |case| case.host = "linux x86_64 6.8.0".to_owned()),
        ("package tree", |case| {
            case.package_tree = tree_digest(&[("src/lib.rs".to_owned(), Digest::of_bytes(b"x"))]);
        }),
        ("declared inputs", |case| {
            case.declared_inputs = tree_digest(&[("../f".to_owned(), Digest::of_bytes(b"f"))]);
        }),
        ("env value", |case| case.env[1].1 = "/opt/bin".to_owned()),
        ("env added", |case| {
            case.env.push(("RUST_LOG".to_owned(), "debug".to_owned()));
        }),
        ("env removed", |case| {
            case.env.pop();
        }),
    ];
    for (name, change) in changes {
        let mut case = Case::new();
        change(&mut case);
        assert_ne!(case.key(), reference, "{name} did not move the key");
    }
}

#[test]
fn color_and_ignored_env_vars_leave_the_key_alone() {
    let reference = Case::new().key();
    let changes: Vec<(&str, fn(&mut Case))> = vec![
        ("--color=", |case| {
            case.args.insert(0, "--color=always".to_owned())
        }),
        ("--color value", |case| {
            case.args.extend(strings(&["--color", "never"]));
        }),
        ("env order", |case| case.env.reverse()),
        ("TERM", |case| {
            case.env.push(("TERM".to_owned(), "xterm".to_owned()))
        }),
        ("PWD", |case| {
            case.env.push(("PWD".to_owned(), "/w/b".to_owned()))
        }),
        ("SSH_ prefix", |case| {
            case.env
                .push(("SSH_AUTH_SOCK".to_owned(), "/tmp/s".to_owned()));
        }),
        ("TINYMOLD_ prefix", |case| {
            case.env
                .push(("TINYMOLD_LOG".to_owned(), "/tmp/l".to_owned()));
        }),
        ("CARGO_MAKEFLAGS", |case| {
            case.env.push((
                "CARGO_MAKEFLAGS".to_owned(),
                "-j --jobserver-fds=3,4".to_owned(),
            ));
        }),
    ];
    for (name, change) in changes {
        let mut case = Case::new();
        change(&mut case);
        assert_eq!(case.key(), reference, "{name} moved the key");
    }
}

#[test]
fn a_second_worktree_shares_the_key() {
    let first = Case::new().key();
    let mut case = Case::new();
    case.env[0].1 = "/w/b/crates/demo".to_owned();
    let other = Roots::new(
        Path::new("/w/b/crates/demo"),
        Path::new("/w/b/target/debug/deps"),
        Some(Path::new("/home/me/.cargo")),
    );
    let second = test_key(&TestKeyInputs {
        binary: &case.binary,
        via: None,
        args: &case.args,
        roots: &other,
        host: &case.host,
        package_tree: &case.package_tree,
        declared_inputs: &case.declared_inputs,
        env: &case.env,
    });
    assert_eq!(first, second);
}

#[test]
fn a_tree_digest_does_not_depend_on_listing_order() {
    let a = ("a".to_owned(), Digest::of_bytes(b"1"));
    let b = ("b".to_owned(), Digest::of_bytes(b"2"));
    assert_eq!(
        tree_digest(&[a.clone(), b.clone()]),
        tree_digest(&[b.clone(), a.clone()])
    );
    // A renamed file, or a moved digest, is a different tree.
    let renamed = ("c".to_owned(), Digest::of_bytes(b"2"));
    assert_ne!(
        tree_digest(&[a.clone(), b.clone()]),
        tree_digest(&[a.clone(), renamed])
    );
    let swapped = [("a".to_owned(), b.1), ("b".to_owned(), a.1)];
    assert_ne!(tree_digest(&[a, b]), tree_digest(&swapped));
}

#[test]
fn ignored_env_names_are_exact_or_prefixed() {
    assert!(is_ignored_env("TERM"));
    assert!(is_ignored_env("SSH_TTY"));
    assert!(is_ignored_env("TINYMOLD_STORE"));
    assert!(!is_ignored_env("TERMINFO"));
    assert!(!is_ignored_env("HOME"));
    assert!(!is_ignored_env("RUST_LOG"));
}
