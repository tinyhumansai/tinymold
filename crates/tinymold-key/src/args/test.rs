//! Unit tests for classifying rustc command lines.

#![allow(clippy::unwrap_used, clippy::panic)]

use super::*;

/// The library compile cargo ran for `grep-matcher`, as captured from a real
/// `cargo build` through `RUSTC_WRAPPER`.
fn library() -> Vec<&'static str> {
    vec![
        "--crate-name",
        "grep_matcher",
        "--edition=2021",
        "crates/matcher/src/lib.rs",
        "--error-format=json",
        "--json=diagnostic-rendered-ansi,artifacts,future-incompat",
        "--crate-type",
        "lib",
        "--emit=dep-info,metadata,link",
        "-C",
        "embed-bitcode=no",
        "-C",
        "debuginfo=2",
        "-C",
        "split-debuginfo=unpacked",
        "-C",
        "metadata=459218bed79010a1",
        "-C",
        "extra-filename=-a538640c85bd477d",
        "--out-dir",
        "/w/rg/target/debug/deps",
        "-C",
        "incremental=/w/rg/target/debug/incremental",
        "-L",
        "dependency=/w/rg/target/debug/deps",
        "--extern",
        "memchr=/w/rg/target/debug/deps/libmemchr-09be5ca3650725ce.rmeta",
    ]
}

fn cacheable(args: &[&str]) -> Invocation {
    match Invocation::parse(args.iter().copied()) {
        Classified::Cacheable(invocation) => *invocation,
        Classified::PassThrough(reason) => panic!("expected cacheable, got {reason}"),
    }
}

#[test]
fn parses_a_real_cargo_library_compile() {
    let invocation = cacheable(&library());
    assert_eq!(invocation.crate_name, "grep_matcher");
    assert_eq!(invocation.crate_types, ["lib"]);
    assert_eq!(invocation.emit, ["dep-info", "metadata", "link"]);
    assert_eq!(invocation.out_dir, PathBuf::from("/w/rg/target/debug/deps"));
    assert_eq!(invocation.extra_filename, "-a538640c85bd477d");
    assert_eq!(
        invocation.source,
        PathBuf::from("crates/matcher/src/lib.rs")
    );
    assert_eq!(
        invocation.externs,
        [Extern {
            name: "memchr".into(),
            path: Some("/w/rg/target/debug/deps/libmemchr-09be5ca3650725ce.rmeta".into())
        }]
    );
    assert_eq!(invocation.search_paths, []);
    assert!(!invocation.links());
    assert_eq!(
        invocation.output_prefixes(),
        [
            "libgrep_matcher-a538640c85bd477d",
            "grep_matcher-a538640c85bd477d"
        ]
    );
    // Where outputs and incremental state go is not part of the key.
    assert!(invocation.key_args.iter().all(|arg| match arg {
        KeyArg::Text(text) => !text.contains("/w/rg/target"),
        _ => true,
    }));
    assert!(
        invocation
            .key_args
            .contains(&KeyArg::Text("-C incremental".into()))
    );
}

#[test]
fn spellings_of_the_same_flag_parse_the_same() {
    let joined = cacheable(&[
        "--crate-name=demo",
        "src/lib.rs",
        "--emit=link",
        "--out-dir=/o",
        "-Cextra-filename=-1",
        "-Lnative=/n",
        "--codegen",
        "opt-level=3",
    ]);
    let split = cacheable(&[
        "--crate-name",
        "demo",
        "src/lib.rs",
        "--emit",
        "link",
        "--out-dir",
        "/o",
        "-C",
        "extra-filename=-1",
        "-L",
        "native=/n",
        "-C",
        "opt-level=3",
    ]);
    assert_eq!(joined, split);
    assert_eq!(
        joined.search_paths,
        [SearchPath {
            kind: "native".into(),
            path: "/n".into()
        }]
    );
}

#[test]
fn linking_units_are_recognized() {
    let base = ["--crate-name", "x", "main.rs", "--out-dir", "/o"];
    let with = |extra: &[&str]| cacheable(&[&base[..], extra].concat());
    assert!(with(&["--crate-type", "bin", "--emit=link"]).links());
    assert!(with(&["--emit=link"]).links());
    assert!(with(&["--crate-type", "proc-macro", "--emit=link"]).links());
    assert!(with(&["--test", "--crate-type", "lib", "--emit=link"]).links());
    assert!(!with(&["--crate-type", "bin", "--emit=dep-info,metadata"]).links());
    assert!(!with(&["--crate-type", "rlib", "--emit=link"]).links());

    let linker = with(&[
        "--emit=link",
        "-C",
        "linker=/usr/bin/cc",
        "-L",
        "/plain",
        "--extern",
        "core",
    ]);
    assert_eq!(linker.linker, Some(PathBuf::from("/usr/bin/cc")));
    assert_eq!(linker.search_paths[0].kind, "all");
    assert_eq!(linker.externs[0].path, None);
}

#[test]
fn passes_through_what_it_cannot_key() {
    let base = [
        "--crate-name",
        "x",
        "lib.rs",
        "--emit=link",
        "--out-dir",
        "/o",
    ];
    let cases: &[(&[&str], &str)] = &[
        (&["-vV"], "asks rustc about itself"),
        (
            &["-", "--crate-name", "___", "--print=file-names"],
            "reads its input from stdin",
        ),
        (&["@/tmp/args"], "reads arguments from a file"),
        (
            &["--crate-name", "x", "lib.rs", "--print", "cfg"],
            "prints compiler information",
        ),
        (&["--crate-name"], "a flag has no value"),
        (&["-C"], "a flag has no value"),
        (
            &[&base[..], &["-o", "out"]].concat(),
            "writes to an explicit output path",
        ),
        (
            &[
                "--crate-name",
                "x",
                "lib.rs",
                "--emit=link=out",
                "--out-dir",
                "/o",
            ],
            "emits to an explicit path",
        ),
        (
            &[&base[..], &["other.rs"]].concat(),
            "has more than one input file",
        ),
        (
            &["--crate-name", "x", "lib.rs", "--out-dir", "/o"],
            "has no --emit",
        ),
        (
            &["lib.rs", "--emit=link", "--out-dir", "/o"],
            "has no --crate-name",
        ),
        (
            &["--crate-name", "x", "lib.rs", "--emit=link"],
            "has no --out-dir",
        ),
        (
            &["--crate-name", "x", "--emit=link", "--out-dir", "/o"],
            "has no input file",
        ),
    ];
    for (args, expected) in cases {
        assert_eq!(
            Invocation::parse(args.iter().copied()),
            Classified::PassThrough(expected),
            "{args:?}"
        );
    }
}

#[test]
fn unknown_and_non_ascii_flags_are_kept_as_text() {
    let invocation = cacheable(&[
        "--crate-name",
        "x",
        "lib.rs",
        "--emit=link",
        "--out-dir",
        "/o",
        "-é",
        "--verbose",
        "-g",
    ]);
    assert!(invocation.key_args.contains(&KeyArg::Text("-é".into())));
    assert!(
        invocation
            .key_args
            .contains(&KeyArg::Text("--verbose".into()))
    );
}

#[test]
fn coverage_instrumentation_is_recognized_in_every_spelling() {
    let base = [
        "--crate-name",
        "demo",
        "src/lib.rs",
        "--emit=link",
        "--out-dir",
        "target",
    ];
    let with = |extra: &[&str]| {
        let mut args = base.to_vec();
        args.extend_from_slice(extra);
        match Invocation::parse(args) {
            Classified::Cacheable(invocation) => invocation.instruments_coverage(),
            Classified::PassThrough(reason) => panic!("{reason}"),
        }
    };
    assert!(with(&["-C", "instrument-coverage"]));
    assert!(with(&["-Cinstrument-coverage=all"]));
    assert!(with(&["--codegen", "instrument-coverage=yes"]));
    assert!(with(&["-Z", "instrument-coverage"]));
    assert!(!with(&["-C", "instrument-coverage=off"]));
    assert!(!with(&["-C", "instrument-coverage=no"]));
    // rustc takes the last setting of a `-C` option.
    assert!(!with(&[
        "-C",
        "instrument-coverage",
        "-C",
        "instrument-coverage=no"
    ]));
    assert!(with(&[
        "-C",
        "instrument-coverage=no",
        "-C",
        "instrument-coverage"
    ]));
    assert!(!with(&[]));
}

#[test]
fn dynamic_linking_of_std_is_recognized_in_every_spelling() {
    let base = [
        "--crate-name",
        "demo",
        "src/lib.rs",
        "--emit=link",
        "--out-dir",
        "target",
    ];
    let with = |extra: &[&str]| {
        let mut args = base.to_vec();
        args.extend_from_slice(extra);
        match Invocation::parse(args) {
            Classified::Cacheable(invocation) => invocation.prefers_dynamic(),
            Classified::PassThrough(reason) => panic!("{reason}"),
        }
    };
    assert!(!with(&[]));
    assert!(with(&["-C", "prefer-dynamic"]));
    assert!(with(&["-Cprefer-dynamic=yes"]));
    assert!(with(&["--codegen", "prefer-dynamic=on"]));
    assert!(!with(&["-C", "prefer-dynamic=no"]));
    assert!(!with(&["-C", "prefer-dynamic", "-C", "prefer-dynamic=off"]));
    assert!(!with(&["-C", "prefer-dynamic-not-really"]));
}
