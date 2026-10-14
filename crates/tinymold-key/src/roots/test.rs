//! Unit tests for root normalization.

use super::*;

fn roots() -> Roots {
    Roots::new(
        Path::new("/work/rg"),
        Path::new("target/debug/deps"),
        Some(Path::new("/home/u/.cargo")),
    )
}

#[test]
fn replaces_the_longest_root_first() {
    let roots = roots();
    assert_eq!(
        roots.normalize("/work/rg/target/debug/deps/librg.rlib"),
        "${OUT}/librg.rlib"
    );
    assert_eq!(
        roots.normalize("-L native=/work/rg/target/debug/build/x-1/out"),
        "-L native=${PROFILE}/build/x-1/out"
    );
    assert_eq!(roots.normalize("/work/rg/src/main.rs"), "${CWD}/src/main.rs");
    assert_eq!(
        roots.normalize("/home/u/.cargo/registry/src/x/lib.rs"),
        "${CARGO_HOME}/registry/src/x/lib.rs"
    );
    assert_eq!(roots.normalize("src/main.rs"), "src/main.rs");
}

#[test]
fn two_worktrees_normalize_to_the_same_text_and_expand_to_their_own() {
    let a = roots();
    let b = Roots::new(Path::new("/work/rg-2"), Path::new("target/debug/deps"), None);
    let text_a = "{\"artifact\":\"/work/rg/target/debug/deps/librg.rmeta\"}";
    let text_b = "{\"artifact\":\"/work/rg-2/target/debug/deps/librg.rmeta\"}";
    assert_eq!(a.normalize(text_a), b.normalize(text_b));
    assert_eq!(b.expand(&a.normalize(text_a)), text_b);
}

#[test]
fn the_profile_directory_is_found_for_deps_examples_and_build_scripts() {
    assert_eq!(
        profile_dir(Path::new("/t/debug/deps")),
        Some(PathBuf::from("/t/debug"))
    );
    assert_eq!(
        profile_dir(Path::new("/t/debug/examples")),
        Some(PathBuf::from("/t/debug"))
    );
    assert_eq!(
        profile_dir(Path::new("/t/debug/build/x-1")),
        Some(PathBuf::from("/t/debug"))
    );
    assert_eq!(profile_dir(Path::new("/t/somewhere")), None);
    assert_eq!(profile_dir(Path::new("/")), None);
}

#[test]
fn a_root_without_a_usable_path_is_skipped() {
    let roots = Roots::new(Path::new("/"), Path::new("/out"), None);
    assert_eq!(roots.normalize("/out/x"), "${OUT}/x");
}
