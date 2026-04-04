use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(i64),
    String(String),
    Array(Vec<JsonValue>),
    Object(BTreeMap<String, JsonValue>),
}

impl JsonValue {
    pub fn string(value: impl Into<String>) -> Self {
        Self::String(value.into())
    }

    pub fn number(value: usize) -> Self {
        Self::Number(value as i64)
    }

    pub fn render_pretty(&self) -> String {
        let mut output = String::new();
        render_value(self, 0, &mut output);
        output.push('\n');
        output
    }

    pub fn as_array(&self) -> Option<&[JsonValue]> {
        match self {
            JsonValue::Array(values) => Some(values),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&BTreeMap<String, JsonValue>> {
        match self {
            JsonValue::Object(values) => Some(values),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsonValue::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            JsonValue::Number(value) => Some(*value),
            _ => None,
        }
    }
}

pub fn parse(text: &str) -> Result<JsonValue, String> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        index: 0,
    };
    let value = parser.parse_value()?;
    parser.skip_whitespace();
    if parser.index != parser.bytes.len() {
        return Err("unexpected trailing content after JSON value".to_string());
    }
    Ok(value)
}

fn render_value(value: &JsonValue, indent: usize, output: &mut String) {
    match value {
        JsonValue::Null => output.push_str("null"),
        JsonValue::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        JsonValue::Number(value) => output.push_str(&value.to_string()),
        JsonValue::String(value) => render_string(value, output),
        JsonValue::Array(values) => {
            if values.is_empty() {
                output.push_str("[]");
                return;
            }

            output.push_str("[\n");
            for (index, item) in values.iter().enumerate() {
                output.push_str(&" ".repeat(indent + 2));
                render_value(item, indent + 2, output);
                if index + 1 != values.len() {
                    output.push(',');
                }
                output.push('\n');
            }
            output.push_str(&" ".repeat(indent));
            output.push(']');
        }
        JsonValue::Object(values) => {
            if values.is_empty() {
                output.push_str("{}");
                return;
            }

            output.push_str("{\n");
            for (index, (key, value)) in values.iter().enumerate() {
                output.push_str(&" ".repeat(indent + 2));
                render_string(key, output);
                output.push_str(": ");
                render_value(value, indent + 2, output);
                if index + 1 != values.len() {
                    output.push(',');
                }
                output.push('\n');
            }
            output.push_str(&" ".repeat(indent));
            output.push('}');
        }
    }
}

fn render_string(value: &str, output: &mut String) {
    output.push('"');
    for ch in value.chars() {
        match ch {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            ch if ch.is_control() => {
                output.push_str(&format!("\\u{:04x}", ch as u32));
            }
            _ => output.push(ch),
        }
    }
    output.push('"');
}

struct Parser<'a> {
    bytes: &'a [u8],
    index: usize,
}

impl<'a> Parser<'a> {
    fn parse_value(&mut self) -> Result<JsonValue, String> {
        self.skip_whitespace();
        let Some(byte) = self.peek() else {
            return Err("unexpected end of JSON input".to_string());
        };

        match byte {
            b'n' => self.parse_null(),
            b't' | b'f' => self.parse_bool(),
            b'-' | b'0'..=b'9' => self.parse_number(),
            b'"' => self.parse_string().map(JsonValue::String),
            b'[' => self.parse_array(),
            b'{' => self.parse_object(),
            _ => Err(format!("unexpected byte `{}` in JSON input", byte as char)),
        }
    }

    fn parse_null(&mut self) -> Result<JsonValue, String> {
        self.expect_bytes(b"null")?;
        Ok(JsonValue::Null)
    }

    fn parse_bool(&mut self) -> Result<JsonValue, String> {
        if self.remaining().starts_with(b"true") {
            self.expect_bytes(b"true")?;
            Ok(JsonValue::Bool(true))
        } else {
            self.expect_bytes(b"false")?;
            Ok(JsonValue::Bool(false))
        }
    }

    fn parse_number(&mut self) -> Result<JsonValue, String> {
        let start = self.index;
        if self.peek() == Some(b'-') {
            self.index += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.index += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.index])
            .map_err(|error| format!("invalid JSON number: {error}"))?;
        let value = text
            .parse::<i64>()
            .map_err(|error| format!("invalid JSON number `{text}`: {error}"))?;
        Ok(JsonValue::Number(value))
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect_byte(b'"')?;
        let mut output = String::new();
        while let Some(byte) = self.peek() {
            self.index += 1;
            match byte {
                b'"' => return Ok(output),
                b'\\' => {
                    let escaped = self
                        .next()
                        .ok_or_else(|| "unterminated escape sequence".to_string())?;
                    match escaped {
                        b'"' => output.push('"'),
                        b'\\' => output.push('\\'),
                        b'/' => output.push('/'),
                        b'b' => output.push('\u{0008}'),
                        b'f' => output.push('\u{000c}'),
                        b'n' => output.push('\n'),
                        b'r' => output.push('\r'),
                        b't' => output.push('\t'),
                        b'u' => {
                            let hex = self.take(4)?;
                            let hex = std::str::from_utf8(hex)
                                .map_err(|error| format!("invalid unicode escape: {error}"))?;
                            let code = u16::from_str_radix(hex, 16).map_err(|error| {
                                format!("invalid unicode escape `{hex}`: {error}")
                            })?;
                            let ch = char::from_u32(code as u32)
                                .ok_or_else(|| format!("invalid unicode scalar `{hex}`"))?;
                            output.push(ch);
                        }
                        other => {
                            return Err(format!("unsupported JSON escape `\\{}`", other as char));
                        }
                    }
                }
                other => output.push(other as char),
            }
        }
        Err("unterminated JSON string".to_string())
    }

    fn parse_array(&mut self) -> Result<JsonValue, String> {
        self.expect_byte(b'[')?;
        let mut values = Vec::new();
        loop {
            self.skip_whitespace();
            if self.peek() == Some(b']') {
                self.index += 1;
                break;
            }
            values.push(self.parse_value()?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.index += 1;
                }
                Some(b']') => {
                    self.index += 1;
                    break;
                }
                _ => return Err("expected `,` or `]` in JSON array".to_string()),
            }
        }
        Ok(JsonValue::Array(values))
    }

    fn parse_object(&mut self) -> Result<JsonValue, String> {
        self.expect_byte(b'{')?;
        let mut values = BTreeMap::new();
        loop {
            self.skip_whitespace();
            if self.peek() == Some(b'}') {
                self.index += 1;
                break;
            }
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect_byte(b':')?;
            let value = self.parse_value()?;
            values.insert(key, value);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.index += 1;
                }
                Some(b'}') => {
                    self.index += 1;
                    break;
                }
                _ => return Err("expected `,` or `}` in JSON object".to_string()),
            }
        }
        Ok(JsonValue::Object(values))
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.index += 1;
        }
    }

    fn expect_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        if self.remaining().starts_with(bytes) {
            self.index += bytes.len();
            Ok(())
        } else {
            Err("unexpected token in JSON input".to_string())
        }
    }

    fn expect_byte(&mut self, byte: u8) -> Result<(), String> {
        if self.peek() == Some(byte) {
            self.index += 1;
            Ok(())
        } else {
            Err(format!("expected byte `{}` in JSON input", byte as char))
        }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        if self.index + len > self.bytes.len() {
            return Err("unexpected end of JSON input".to_string());
        }
        let slice = &self.bytes[self.index..self.index + len];
        self.index += len;
        Ok(slice)
    }

    fn remaining(&self) -> &'a [u8] {
        &self.bytes[self.index..]
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.index += 1;
        Some(byte)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{JsonValue, parse};

    #[test]
    fn parses_arrays_and_objects() {
        let value = parse(r#"[{"path":"src/main.rs","line":12},null]"#).unwrap();
        let array = value.as_array().unwrap();
        let object = array[0].as_object().unwrap();
        assert_eq!(
            object.get("path").and_then(JsonValue::as_str),
            Some("src/main.rs")
        );
        assert_eq!(object.get("line").and_then(JsonValue::as_i64), Some(12));
    }

    #[test]
    fn renders_pretty_json() {
        let mut object = BTreeMap::new();
        object.insert("path".to_string(), JsonValue::string("src/main.rs"));
        object.insert("line".to_string(), JsonValue::Number(7));
        let text = JsonValue::Object(object).render_pretty();
        assert!(text.contains("\"path\": \"src/main.rs\""));
    }
}
