//! A one-line JSON object builder for the match log — flat objects and
//! string escaping per RFC 8259. Nothing here needs what a serialisation
//! framework offers, and the graph's third-party surface is scanned before
//! a release, so it stays small on purpose.

use std::fmt::Write as _;

#[derive(Debug)]
pub struct JsonObject {
    buf: String,
    first: bool,
}

impl JsonObject {
    #[must_use]
    pub fn new() -> Self {
        Self {
            buf: String::from("{"),
            first: true,
        }
    }

    fn key(&mut self, key: &str) {
        if !self.first {
            self.buf.push(',');
        }
        self.first = false;
        let _ = write!(self.buf, "\"{}\":", escape(key));
    }

    #[must_use]
    pub fn str(mut self, key: &str, value: &str) -> Self {
        self.key(key);
        let _ = write!(self.buf, "\"{}\"", escape(value));
        self
    }

    #[must_use]
    pub fn u64(mut self, key: &str, value: u64) -> Self {
        self.key(key);
        let _ = write!(self.buf, "{value}");
        self
    }

    #[must_use]
    pub fn f64(mut self, key: &str, value: f64) -> Self {
        self.key(key);
        // JSON has no NaN/Infinity; the log writes them as strings so a
        // reader chokes loudly rather than misparsing.
        if value.is_finite() {
            let _ = write!(self.buf, "{value}");
        } else {
            let _ = write!(self.buf, "\"{value}\"");
        }
        self
    }

    #[must_use]
    pub fn bool(mut self, key: &str, value: bool) -> Self {
        self.key(key);
        let _ = write!(self.buf, "{value}");
        self
    }

    #[must_use]
    pub fn str_array(mut self, key: &str, values: &[String]) -> Self {
        self.key(key);
        self.buf.push('[');
        for (i, v) in values.iter().enumerate() {
            if i > 0 {
                self.buf.push(',');
            }
            let _ = write!(self.buf, "\"{}\"", escape(v));
        }
        self.buf.push(']');
        self
    }

    /// The finished line, no trailing newline.
    #[must_use]
    pub fn finish(mut self) -> String {
        self.buf.push('}');
        self.buf
    }
}

impl Default for JsonObject {
    fn default() -> Self {
        Self::new()
    }
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_object_line_carries_every_field_type() {
        let line = JsonObject::new()
            .str("type", "game")
            .u64("pair", 7)
            .f64("llr", 0.5)
            .bool("candidate_black", true)
            .str_array("tail", &["a".to_owned(), "b".to_owned()])
            .finish();
        assert_eq!(
            line,
            r#"{"type":"game","pair":7,"llr":0.5,"candidate_black":true,"tail":["a","b"]}"#
        );
    }

    #[test]
    fn strings_with_quotes_backslashes_and_control_bytes_stay_one_line() {
        let line = JsonObject::new().str("s", "a\"b\\c\nd\te\u{1}").finish();
        assert_eq!(line, "{\"s\":\"a\\\"b\\\\c\\nd\\te\\u0001\"}");
        assert!(!line.contains('\n'));
    }

    #[test]
    fn a_non_finite_number_becomes_a_string_not_invalid_json() {
        let line = JsonObject::new().f64("llr", f64::INFINITY).finish();
        assert_eq!(line, r#"{"llr":"inf"}"#);
    }
}
