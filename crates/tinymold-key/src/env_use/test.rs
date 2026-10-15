//! Unit tests for reading how source text uses an env var.

use super::*;

fn picks_files(source: &str) -> bool {
    EnvUse::scan(source.as_bytes(), "OUT_DIR").only_picks_included_files()
}

#[test]
fn the_include_idiom_only_picks_files() {
    assert!(picks_files(
        "include!(concat!(env!(\"OUT_DIR\"), \"/generated.rs\"));\n"
    ));
    assert!(picks_files(
        "pub const V: &str = include_str!(concat!(env!(\"OUT_DIR\"), \"/v.txt\"));\n\
         static B: &[u8] = ::core::include_bytes!(concat!(env!(\"OUT_DIR\"), \"/\", \"b.bin\",));\n"
    ));
    assert!(picks_files(
        "const V: Version = include!(concat!(env!(\"OUT_DIR\"), \"\\\\version.expr\"));\n"
    ));
    assert!(picks_files(
        "include ! (\n    concat ! ( env ! ( \"OUT_DIR\" ) ,\n \"/x.rs\" )\n);\n"
    ));
}

#[test]
fn mentions_on_comment_lines_are_not_uses() {
    assert!(picks_files(
        "//! Run `cargo build`, which writes `OUT_DIR/x.rs`:\n\
         //! let n = env!(\"OUT_DIR\").len();\n\
         include!(concat!(env!(\"OUT_DIR\"), \"/x.rs\"));\n"
    ));
}

#[test]
fn any_other_use_is_not_only_picking_files() {
    for source in [
        "pub static LEN: usize = env!(\"OUT_DIR\").len();\n",
        "include!(concat!(env!(\"OUT_DIR\"), \"/x.rs\"));\n\
         pub static LEN: usize = env!(\"OUT_DIR\").len();\n",
        "include!(concat!(env!(\"OUT_DIR\"), \"/x.rs\"));\n\
         pub const P: Option<&str> = option_env!(\"OUT_DIR\");\n",
        "include!(concat!(env!(\"OUT_DIR\"), \"/x.rs\")); const P: &str = env!(\"OUT_DIR\");\n",
        // The suffix must be literal text, not another expression.
        "include!(concat!(env!(\"OUT_DIR\"), SUFFIX));\n",
        "include!(concat!(env!(\"OUT_DIR\")));\n",
        "my_include!(concat!(env!(\"OUT_DIR\"), \"/x.rs\"));\n",
        "/* OUT_DIR */ include!(concat!(env!(\"OUT_DIR\"), \"/x.rs\"));\n",
        "",
    ] {
        assert!(!picks_files(source), "{source}");
    }
}

#[test]
fn a_name_the_text_cannot_see_is_opaque() {
    for source in [
        "include!(concat!(env!(\"OUT_DIR\"), \"/x.rs\"));\n\
         static L: usize = env!(concat!(\"OUT\", \"_DIR\")).len();\n",
        "include!(concat!(env!(\"OUT_DIR\"), \"/x.rs\"));\n\
         static L: usize = env!(\"OUT\\x5fDIR\").len();\n",
        "include!(concat!(env!(\"OUT_DIR\"), \"/x.rs\"));\n\
         static L: usize = env!(r\"OUT_DIR\").len();\n",
        "include!(concat!(env!(\"OUT_DIR\"), \"/x.rs\"));\n\
         macro_rules! var { ($n:literal) => { env!($n) } }\n",
        "macro_rules! include { ($e:expr) => { static L: usize = $e.len(); } }\n\
         include!(concat!(env!(\"OUT_DIR\"), \"/x.rs\"));\n",
    ] {
        let found = EnvUse::scan(source.as_bytes(), "OUT_DIR");
        assert!(found.opaque, "{source}");
        assert!(!found.only_picks_included_files(), "{source}");
    }
}

#[test]
fn text_that_is_not_an_env_invocation_is_not_opaque() {
    let found = EnvUse::scan(
        b"use std::env;\nfn f() -> String { std::env::var(\"HOME\").unwrap() }\n\
          const M: &str = env!(\"CARGO_PKG_NAME\", \"no name\");\n",
        "OUT_DIR",
    );
    assert_eq!(found, EnvUse::default());
}

#[test]
fn uses_add_up_across_the_files_of_one_compile() {
    let lib = EnvUse::scan(
        b"include!(concat!(env!(\"OUT_DIR\"), \"/x.rs\"));\n",
        "OUT_DIR",
    );
    let generated = EnvUse::scan(b"pub const OUT_DIR_LEN: usize = 3;\n", "OUT_DIR");
    assert!(lib.merge(generated).only_picks_included_files());
    let leaky = EnvUse::scan(b"static L: usize = env!(\"OUT_DIR\").len();\n", "OUT_DIR");
    assert!(!lib.merge(leaky).only_picks_included_files());
    assert!(!lib.merge(EnvUse {
        opaque: true,
        ..EnvUse::default()
    })
    .only_picks_included_files());
}
