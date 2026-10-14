//! Unit tests for the pre-key and full key.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::Path;

use super::*;
use crate::Classified;

fn invocation(args: &[&str]) -> Invocation {
    match Invocation::parse(args.iter().copied()) {
        Classified::Cacheable(invocation) => *invocation,
        Classified::PassThrough(reason) => panic!("{reason}"),
    }
}

/// A library compile in the worktree at `root`.
fn compile(root: &str, extra: &[&str]) -> (Invocation, Roots) {
    let out = format!("{root}/target/debug/deps");
    let incremental = format!("-Cincremental={root}/target/debug/incremental");
    let dependency = format!("dependency={out}");
    let ext = format!("dep={out}/libdep-1.rmeta");
    let native = format!("native={root}/target/debug/build/dep-2/out");
    let mut args = vec![
        "--crate-name",
        "demo",
        "src/lib.rs",
        "--crate-type",
        "lib",
        "--emit=dep-info,link",
        "--out-dir",
        &out,
        &incremental,
        "-L",
        &dependency,
        "--extern",
        &ext,
        "-L",
        &native,
    ];
    args.extend_from_slice(extra);
    let invocation = invocation(&args);
    let roots = Roots::new(Path::new(root), &invocation.out_dir, None);
    (invocation, roots)
}

fn key_of(invocation: &Invocation, roots: &Roots, extern_digest: &[u8]) -> Digest {
    pre_key(&PreKeyInputs {
        invocation,
        roots,
        rustc_version: "rustc 1.99.0",
        env: &[("SDKROOT".into(), None)],
        externs: &[Some(Digest::of_bytes(extern_digest))],
        search_paths: &[vec![("libnative.a".into(), Digest::of_bytes(b"archive"))]],
        linker: None,
    })
}

#[test]
fn the_pre_key_encoding_is_pinned() {
    let (invocation, roots) = compile("/w/a", &[]);
    // Changing the encoding orphans every cache entry ever written, so this
    // value only changes together with the schema tag.
    assert_eq!(
        key_of(&invocation, &roots, b"rmeta").to_hex(),
        PINNED_PRE_KEY
    );
    let full = full_key(
        &Digest::of_bytes(b""),
        &[("src/lib.rs".into(), Digest::of_bytes(b"fn f() {}"))],
        &[("CARGO_PKG_VERSION".into(), Some("0.1.0".into()))],
    );
    assert_eq!(full.to_hex(), PINNED_FULL_KEY);
}

const PINNED_PRE_KEY: &str = "ceef4d7894b78c9e80396d277dd0d2d51ac7dde86634fbcade94d9da24d0dfb1";
const PINNED_FULL_KEY: &str = "0928ee02526ad8465c56c90f19c168211dee5e0f8467911165ac6c580def0de6";

#[test]
fn two_worktrees_of_one_compile_share_a_pre_key() {
    let (a, roots_a) = compile("/w/a", &[]);
    let (b, roots_b) = compile("/w/second-worktree", &[]);
    assert_eq!(
        key_of(&a, &roots_a, b"rmeta"),
        key_of(&b, &roots_b, b"rmeta")
    );
}

#[test]
fn every_input_moves_the_pre_key() {
    let (base, roots) = compile("/w/a", &[]);
    let reference = key_of(&base, &roots, b"rmeta");
    assert_ne!(
        key_of(&base, &roots, b"other rmeta"),
        reference,
        "extern content"
    );
    let (flagged, roots_f) = compile("/w/a", &["-C", "opt-level=3"]);
    assert_ne!(key_of(&flagged, &roots_f, b"rmeta"), reference, "a flag");

    let env = [("SDKROOT".to_owned(), None)];
    let externs = [Some(Digest::of_bytes(b"rmeta"))];
    let search = [vec![(
        "libnative.a".to_owned(),
        Digest::of_bytes(b"archive"),
    )]];
    let defaults = PreKeyInputs {
        invocation: &base,
        roots: &roots,
        rustc_version: "rustc 1.99.0",
        env: &env,
        externs: &externs,
        search_paths: &search,
        linker: None,
    };
    assert_eq!(pre_key(&defaults), reference);

    let set = [("SDKROOT".to_owned(), Some("/sdk".to_owned()))];
    let native = [vec![(
        "libnative.a".to_owned(),
        Digest::of_bytes(b"changed"),
    )]];
    let changes = [
        (
            "compiler",
            PreKeyInputs {
                rustc_version: "rustc 1.100.0",
                ..defaults.clone()
            },
        ),
        (
            "env",
            PreKeyInputs {
                env: &set,
                ..defaults.clone()
            },
        ),
        (
            "missing extern",
            PreKeyInputs {
                externs: &[None],
                ..defaults.clone()
            },
        ),
        (
            "native lib",
            PreKeyInputs {
                search_paths: &native,
                ..defaults.clone()
            },
        ),
        (
            "no search listing",
            PreKeyInputs {
                search_paths: &[],
                ..defaults.clone()
            },
        ),
        (
            "linker",
            PreKeyInputs {
                linker: Some("cc 1"),
                ..defaults.clone()
            },
        ),
    ];
    for (what, inputs) in changes {
        assert_ne!(pre_key(&inputs), reference, "{what}");
    }
}

#[test]
fn every_input_moves_the_full_key() {
    let pre = Digest::of_bytes(b"pre");
    let inputs = [("src/lib.rs".to_owned(), Digest::of_bytes(b"a"))];
    let env = [("V".to_owned(), Some("1".to_owned()))];
    let reference = full_key(&pre, &inputs, &env);
    assert_ne!(full_key(&Digest::of_bytes(b"x"), &inputs, &env), reference);
    assert_ne!(
        full_key(
            &pre,
            &[("src/lib.rs".to_owned(), Digest::of_bytes(b"b"))],
            &env
        ),
        reference
    );
    assert_ne!(
        full_key(&pre, &inputs, &[("V".to_owned(), None)]),
        reference
    );
    assert_ne!(full_key(&pre, &[], &env), reference);
}
