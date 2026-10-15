//! Unit tests for the `GOCACHEPROG` adapter, driven over in-memory input
//! and output.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use base64::Engine as _;

use super::message::parse_request;
use super::*;

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("server")).unwrap();
    (dir, store)
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Runs a session on `input` and returns the response lines.
fn session(store: &Store, input: &str) -> Vec<String> {
    let mut output = Vec::new();
    serve(store, input.as_bytes(), &mut output).unwrap();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn put_line(id: i64, action: &[u8], output: &[u8], body: &[u8]) -> String {
    format!(
        "{{\"ID\":{id},\"Command\":\"put\",\"ActionID\":\"{}\",\"OutputID\":\"{}\",\"BodySize\":{}}}\n\"{}\"\n",
        b64(action),
        b64(output),
        body.len(),
        b64(body)
    )
}

fn get_line(id: i64, action: &[u8]) -> String {
    format!(
        "{{\"ID\":{id},\"Command\":\"get\",\"ActionID\":\"{}\"}}\n",
        b64(action)
    )
}

/// The `DiskPath` of a response line.
fn disk_path(line: &str) -> String {
    let start = line.find("\"DiskPath\":\"").unwrap() + "\"DiskPath\":\"".len();
    let end = line[start..].find('"').unwrap();
    line[start..start + end].to_owned()
}

#[test]
fn a_session_starts_by_announcing_its_commands() {
    let (_dir, store) = store();

    assert_eq!(
        session(&store, ""),
        [r#"{"ID":0,"KnownCommands":["get","put","close"]}"#]
    );
}

#[test]
fn an_unknown_action_is_a_miss() {
    let (_dir, store) = store();

    let lines = session(&store, &get_line(1, b"never stored"));

    assert_eq!(lines[1], r#"{"ID":1,"Miss":true}"#);
}

#[test]
fn a_put_output_is_served_back_from_its_disk_path() {
    let (_dir, store) = store();
    let input = put_line(1, b"action", b"output-id", b"compiled package") + &get_line(2, b"action");

    let lines = session(&store, &input);

    let stored = disk_path(&lines[1]);
    assert_eq!(std::fs::read(&stored).unwrap(), b"compiled package");
    assert!(lines[2].starts_with(r#"{"ID":2,"OutputID":""#), "{}", lines[2]);
    assert!(lines[2].contains(&b64(b"output-id")));
    assert!(lines[2].contains(r#""Size":16"#));
    assert_eq!(disk_path(&lines[2]), stored);
}

#[test]
fn an_empty_output_has_no_body_line() {
    let (_dir, store) = store();
    let input = format!(
        "{{\"ID\":1,\"Command\":\"put\",\"ActionID\":\"{}\",\"OutputID\":\"{}\"}}\n{}",
        b64(b"empty"),
        b64(b"out"),
        get_line(2, b"empty")
    );

    let lines = session(&store, &input);

    assert_eq!(std::fs::read(disk_path(&lines[1])).unwrap(), b"");
    assert!(lines[2].contains(r#""Size":0"#));
}

#[test]
fn a_body_of_the_wrong_size_is_refused_and_the_session_stays_in_step() {
    let (_dir, store) = store();
    let wrong = format!(
        "{{\"ID\":1,\"Command\":\"put\",\"ActionID\":\"{}\",\"BodySize\":99}}\n\"{}\"\n",
        b64(b"action"),
        b64(b"short")
    );
    let input = wrong + &get_line(2, b"action");

    let lines = session(&store, &input);

    assert!(lines[1].starts_with(r#"{"ID":1,"Err":"#), "{}", lines[1]);
    assert_eq!(lines[2], r#"{"ID":2,"Miss":true}"#);
}

#[test]
fn unknown_commands_are_answered_with_an_error() {
    let (_dir, store) = store();

    let lines = session(&store, "{\"ID\":7,\"Command\":\"stat\"}\n");

    assert_eq!(lines[1], r#"{"ID":7,"Err":"unknown command"}"#);
}

#[test]
fn close_answers_and_ends_the_session() {
    let (_dir, store) = store();
    let input = "{\"ID\":3,\"Command\":\"close\"}\n".to_owned() + &get_line(4, b"after");

    let lines = session(&store, &input);

    assert_eq!(lines[1..], [r#"{"ID":3}"#]);
}

#[test]
fn blank_lines_between_requests_are_skipped() {
    let (_dir, store) = store();
    let input = "\n\n".to_owned() + &get_line(1, b"x");

    assert_eq!(session(&store, &input).len(), 2);
}

#[test]
fn input_that_is_not_the_protocol_ends_the_session_with_an_error() {
    let (_dir, store) = store();
    let mut output = Vec::new();

    let error = serve(&store, &b"not json\n"[..], &mut output).unwrap_err();

    assert!(matches!(error, Error::CacheProg(_)), "{error:?}");
}

#[test]
fn a_body_that_is_not_a_json_string_is_an_input_error() {
    let (_dir, store) = store();
    let input = format!(
        "{{\"ID\":1,\"Command\":\"put\",\"ActionID\":\"{}\",\"BodySize\":3}}\nnot a string\n",
        b64(b"action")
    );
    let mut output = Vec::new();

    let error = serve(&store, input.as_bytes(), &mut output).unwrap_err();

    assert!(matches!(error, Error::Input(_)), "{error:?}");
}

#[test]
fn a_body_line_cut_off_mid_string_is_an_input_error() {
    let (_dir, store) = store();
    let input = format!(
        "{{\"ID\":1,\"Command\":\"put\",\"ActionID\":\"{}\",\"BodySize\":3}}\n\"YWJj",
        b64(b"action")
    );
    let mut output = Vec::new();

    let error = serve(&store, input.as_bytes(), &mut output).unwrap_err();

    assert!(matches!(error, Error::Input(_)), "{error:?}");
}

#[test]
fn a_closed_output_is_an_output_error() {
    struct Closed;
    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let (_dir, store) = store();

    assert!(matches!(
        serve(&store, &b""[..], Closed),
        Err(Error::Output(_))
    ));
}

#[test]
fn requests_parse_with_escapes_and_the_old_object_id_name() {
    let request = parse_request(
        r#" {"ID": 12, "Command": "put", "ObjectID": "AQI=", "BodySize": 5, "Extra": true, "Gone": null} "#,
    )
    .unwrap();

    assert_eq!(request.id, 12);
    assert_eq!(request.command, "put");
    assert_eq!(request.output_id, [1, 2]);
    assert_eq!(request.body_size, 5);
}

#[test]
fn malformed_requests_are_refused() {
    for line in [
        r#"{"ID":1}"#,
        r#"{"ID":"one","Command":"get"}"#,
        r#"{"ID":1,"Command":"get","ActionID":"!!"}"#,
        r#"{"ID":1,"Command":"put","BodySize":-1}"#,
        r#"{"ID":1,"Command":"get","Nested":{"a":1}}"#,
        r#"{"ID":1,"Command":"get"} trailing"#,
        r#"{"ID":1,"Command":"get"#,
        r#"{"ID":1,"Command":"g\qet"}"#,
        r#"{"ID":1,"Command":"g\u00"}"#,
        r#"["ID",1]"#,
    ] {
        assert!(parse_request(line).is_err(), "{line}");
    }
    assert!(parse_request("{}").is_err());
}

#[test]
fn json_strings_escape_quotes_backslashes_and_controls() {
    assert_eq!(
        json_string("a\"b\\c\nd"),
        r#""a\"b\\c\u000ad""#
    );
}

#[test]
fn hex_round_trips_and_rejects_odd_or_bad_text() {
    assert_eq!(hex_decode(&hex_encode(&[0, 171, 255])), Some(vec![0, 171, 255]));
    assert_eq!(hex_decode("abc"), None);
    assert_eq!(hex_decode("zz"), None);
}
