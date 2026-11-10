//! A minimal JSON value and writer, enough for residue's output documents.

use std::path::Path;

pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    /// A number that may have a fraction, such as a percentage. One that
    /// is not finite, which JSON cannot hold, is written as null.
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(&'static str, Json)>),
}

impl Json {
    pub fn str(s: impl Into<String>) -> Json {
        Json::Str(s.into())
    }

    /// Paths that are not valid UTF-8 are written lossily (U+FFFD).
    pub fn path(p: &Path) -> Json {
        Json::Str(p.to_string_lossy().into_owned())
    }

    pub fn opt<T>(value: Option<T>, f: impl FnOnce(T) -> Json) -> Json {
        value.map_or(Json::Null, f)
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Int(n) => out.push_str(&n.to_string()),
            Json::UInt(n) => out.push_str(&n.to_string()),
            // Rust prints a finite f64 as plain digits, never with an
            // exponent: `10`, `39.25`, `0.0001`.
            Json::Num(n) if n.is_finite() => out.push_str(&n.to_string()),
            Json::Num(_) => out.push_str("null"),
            Json::Str(s) => escape(s, out),
            Json::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Obj(fields) => {
                out.push('{');
                for (i, (key, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    escape(key, out);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

fn escape(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}
