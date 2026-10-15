//! Unit tests for reading a package's declared test inputs.

use super::*;

fn inputs(paths: &[&str]) -> Declared {
    Declared::Inputs(paths.iter().map(|path| (*path).to_owned()).collect())
}

#[test]
fn a_manifest_without_the_table_declares_nothing() {
    let manifest = "[package]\nname = \"tinymold-demo\"\n\n[dependencies]\nserde = \"1\"\n";
    assert_eq!(parse(manifest), inputs(&[]));
}

#[test]
fn reads_inputs_on_one_line_or_several() {
    let one = "[package]\nname = \"a\"\n[package.metadata.tinymold.test]\ninputs = [\"../fixtures\", \"../schema.sql\"]\n";
    assert_eq!(parse(one), inputs(&["../fixtures", "../schema.sql"]));
    let several = "[package.metadata.tinymold.test]\n# fixtures the tests read\ninputs = [\n  \"../fixtures\", # all of them\n  \"data.bin\",\n]\ncache = true\n\n[dependencies]\n";
    assert_eq!(parse(several), inputs(&["../fixtures", "data.bin"]));
}

#[test]
fn cache_false_opts_the_package_out() {
    let manifest = "[package.metadata.tinymold.test]\ncache = false\n";
    assert_eq!(
        parse(manifest),
        Declared::Uncached("the package sets cache = false")
    );
}

#[test]
fn anything_it_cannot_read_for_certain_is_uncached() {
    let unreadable = [
        // Other spellings of the same table.
        "[package.metadata.tinymold]\ntest = { inputs = [\"x\"] }\n",
        "[package.metadata]\ntinymold.test.inputs = [\"x\"]\n",
        "[package]\nmetadata.tinymold.test.inputs = [\"x\"]\n",
        "[package]\nmetadata = { tinymold = { test = { cache = false } } }\n",
        "[ package.metadata.tinymold.test ]\ncache = false\n",
        // Dotted keys and inline tables at the document root.
        "package.metadata.tinymold.test.inputs = [\"../fixtures\"]\n[package]\nname = \"a\"\n",
        "package = { name = \"a\", metadata = { tinymold = { test = { cache = false } } } }\n",
        "\"package\".\"metadata\".tinymold.test.cache = false\n",
        "[package]\n\"metadata\" . tinymold.test.cache = false\n",
        // Values outside the subset.
        "[package.metadata.tinymold.test]\ninputs = ['x']\n",
        "[package.metadata.tinymold.test]\ninputs = [\"a\\\"b\"]\n",
        "[package.metadata.tinymold.test]\ninputs = \"x\"\n",
        "[package.metadata.tinymold.test]\ninputs = [\"x\"\n",
        "[package.metadata.tinymold.test]\ninputs = [\"x\" \"y\"]\n",
        "[package.metadata.tinymold.test]\ncache = maybe\n",
        "[package.metadata.tinymold.test]\nenv = [\"X\"]\n",
        // Absolute paths name one machine.
        "[package.metadata.tinymold.test]\ninputs = [\"/etc/hosts\"]\n",
    ];
    for manifest in unreadable {
        assert!(
            matches!(parse(manifest), Declared::Uncached(_)),
            "{manifest}"
        );
    }
}

#[test]
fn a_dependency_named_tinymold_is_not_metadata() {
    let manifest = "[dependencies]\ntinymold = \"0.1\"\n[package]\nname = \"tinymold-demo\"\n";
    assert_eq!(parse(manifest), inputs(&[]));
}
