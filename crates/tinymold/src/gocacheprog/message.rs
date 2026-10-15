//! The `GOCACHEPROG` wire format: one JSON object per line.
//!
//! The go command's requests are flat objects of strings, numbers and
//! booleans, with byte fields (`ActionID`, `OutputID`) base64-encoded as
//! Go's `encoding/json` writes `[]byte`. A `put` with a body is followed by
//! one more line holding the body as a base64 JSON string. The protocol is
//! defined in Go's `cmd/go/internal/cacheprog`.
//!
//! This module parses exactly that shape and writes the responses; a
//! request with nested objects or arrays is refused rather than guessed at.

use std::io::{self, BufRead, Read};

use base64::Engine as _;

/// A request from the go command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) id: i64,
    pub(crate) command: String,
    pub(crate) action_id: Vec<u8>,
    pub(crate) output_id: Vec<u8>,
    pub(crate) body_size: u64,
}

/// One JSON value of a flat object.
#[derive(Debug, Clone, PartialEq)]
enum Value {
    Text(String),
    Number(i64),
    Other,
}

/// Parses one request line.
pub(crate) fn parse_request(line: &str) -> Result<Request, &'static str> {
    let mut request = Request::default();
    for (name, value) in parse_object(line)? {
        match (name.as_str(), value) {
            ("ID", Value::Number(id)) => request.id = id,
            ("Command", Value::Text(command)) => request.command = command,
            ("ActionID", Value::Text(text)) => request.action_id = decode(&text)?,
            // `ObjectID` is what Go called `OutputID` before 1.24.
            ("OutputID" | "ObjectID", Value::Text(text)) => request.output_id = decode(&text)?,
            ("BodySize", Value::Number(size)) => {
                request.body_size = u64::try_from(size).map_err(|_| "negative BodySize")?;
            }
            ("ID" | "Command" | "ActionID" | "OutputID" | "ObjectID" | "BodySize", _) => {
                return Err("a request field has the wrong type");
            }
            _ => {}
        }
    }
    if request.command.is_empty() {
        return Err("a request has no Command");
    }
    Ok(request)
}

fn decode(text: &str) -> Result<Vec<u8>, &'static str> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|_| "a byte field is not base64")
}

/// Parses a flat JSON object into its fields, in order.
fn parse_object(line: &str) -> Result<Vec<(String, Value)>, &'static str> {
    let mut cursor = Cursor(line.trim().as_bytes());
    cursor.expect(b'{')?;
    let mut fields = Vec::new();
    cursor.skip_space();
    if cursor.eat(b'}') {
        return cursor.end().map(|()| fields);
    }
    loop {
        cursor.skip_space();
        let name = cursor.string()?;
        cursor.skip_space();
        cursor.expect(b':')?;
        cursor.skip_space();
        let value = cursor.value()?;
        fields.push((name, value));
        cursor.skip_space();
        if cursor.eat(b'}') {
            return cursor.end().map(|()| fields);
        }
        cursor.expect(b',')?;
    }
}

struct Cursor<'a>(&'a [u8]);

impl Cursor<'_> {
    fn skip_space(&mut self) {
        while let [b' ' | b'\t' | b'\r' | b'\n', rest @ ..] = self.0 {
            self.0 = rest;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        match self.0 {
            [first, rest @ ..] if *first == byte => {
                self.0 = rest;
                true
            }
            _ => false,
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), &'static str> {
        if self.eat(byte) {
            Ok(())
        } else {
            Err("a request is not a flat JSON object")
        }
    }

    fn end(&self) -> Result<(), &'static str> {
        if self.0.iter().all(u8::is_ascii_whitespace) {
            Ok(())
        } else {
            Err("a request has text after its object")
        }
    }

    fn value(&mut self) -> Result<Value, &'static str> {
        match self.0.first() {
            Some(b'"') => self.string().map(Value::Text),
            Some(b'-' | b'0'..=b'9') => {
                let len = self
                    .0
                    .iter()
                    .position(|byte| !(byte.is_ascii_digit() || *byte == b'-'))
                    .unwrap_or(self.0.len());
                let (number, rest) = self.0.split_at(len);
                self.0 = rest;
                std::str::from_utf8(number)
                    .ok()
                    .and_then(|number| number.parse().ok())
                    .map(Value::Number)
                    .ok_or("a request has a malformed number")
            }
            _ => {
                for word in [&b"true"[..], b"false", b"null"] {
                    if let Some(rest) = self.0.strip_prefix(word) {
                        self.0 = rest;
                        return Ok(Value::Other);
                    }
                }
                Err("a request field is not a string, number or boolean")
            }
        }
    }

    fn string(&mut self) -> Result<String, &'static str> {
        self.expect(b'"')?;
        let mut out = Vec::new();
        loop {
            let [byte, rest @ ..] = self.0 else {
                return Err("a request has an unterminated string");
            };
            self.0 = rest;
            match byte {
                b'"' => return String::from_utf8(out).map_err(|_| "a request string is not utf-8"),
                b'\\' => {
                    let [escape, rest @ ..] = self.0 else {
                        return Err("a request has an unterminated string");
                    };
                    self.0 = rest;
                    match escape {
                        b'"' | b'\\' | b'/' => out.push(*escape),
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'u' => {
                            let (hex, rest) = self
                                .0
                                .split_at_checked(4)
                                .ok_or("a request has a short \\u escape")?;
                            self.0 = rest;
                            let code = std::str::from_utf8(hex)
                                .ok()
                                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                                .and_then(char::from_u32)
                                .ok_or("a request has a bad \\u escape")?;
                            let mut buffer = [0_u8; 4];
                            out.extend_from_slice(code.encode_utf8(&mut buffer).as_bytes());
                        }
                        _ => return Err("a request has an unknown escape"),
                    }
                }
                other => out.push(*other),
            }
        }
    }
}

/// Reads the body line that follows a `put`: a JSON string of base64,
/// yielding the base64 text without its quotes, and consuming the line.
pub(crate) struct BodyLine<'a, R> {
    input: &'a mut R,
    started: bool,
    done: bool,
}

impl<'a, R: BufRead> BodyLine<'a, R> {
    pub(crate) fn new(input: &'a mut R) -> Self {
        Self {
            input,
            started: false,
            done: false,
        }
    }

    /// The decoded body, as a reader.
    pub(crate) fn decoded(&mut self) -> impl Read + '_ {
        base64::read::DecoderReader::new(self, &base64::engine::general_purpose::STANDARD)
    }

    /// Reads whatever is left of the line, so the next request starts in
    /// step even when the body was not read to its end.
    pub(crate) fn finish(&mut self) -> io::Result<()> {
        let mut sink = [0_u8; 8192];
        while self.read(&mut sink)? != 0 {}
        Ok(())
    }
}

impl<R: BufRead> Read for BodyLine<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let invalid = |message| io::Error::new(io::ErrorKind::InvalidData, message);
        if self.done {
            return Ok(0);
        }
        if !self.started {
            // Skip to the opening quote.
            loop {
                let available = self.input.fill_buf()?;
                let Some(&first) = available.first() else {
                    return Err(invalid("the body line ended before it began"));
                };
                self.input.consume(1);
                match first {
                    b'"' => break,
                    b' ' | b'\t' | b'\r' | b'\n' => {}
                    _ => return Err(invalid("a body is not a JSON string")),
                }
            }
            self.started = true;
        }
        let available = self.input.fill_buf()?;
        if available.is_empty() {
            return Err(invalid("the body line ended inside its string"));
        }
        match available.iter().position(|byte| *byte == b'"') {
            Some(0) => {
                self.input.consume(1);
                // The rest of the line: only the newline is expected.
                let mut rest = String::new();
                self.input.read_line(&mut rest)?;
                if !rest.trim().is_empty() {
                    return Err(invalid("a body line has text after its string"));
                }
                self.done = true;
                Ok(0)
            }
            end => {
                let len = end.unwrap_or(available.len()).min(buffer.len());
                buffer[..len].copy_from_slice(&available[..len]);
                self.input.consume(len);
                Ok(len)
            }
        }
    }
}

/// `text` as a JSON string literal.
pub(crate) fn json_string(text: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            control if control.is_control() => {
                let _ = write!(out, "\\u{:04x}", u32::from(control));
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// `bytes` as Go's `encoding/json` writes a `[]byte`.
pub(crate) fn json_bytes(bytes: &[u8]) -> String {
    format!(
        "\"{}\"",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}
