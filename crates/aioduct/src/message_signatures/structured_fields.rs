use base64::Engine as _;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StructuredFieldError {
    Parse,
}

pub(crate) fn dictionary_member(
    input: &str,
    key: &str,
) -> Result<Option<String>, StructuredFieldError> {
    if !input.is_ascii() {
        return Err(StructuredFieldError::Parse);
    }

    let mut parser = Parser::new(input);
    parser.discard_sp();
    let member = parser.parse_dictionary(key)?;
    parser.discard_sp();
    if !parser.is_empty() {
        return Err(StructuredFieldError::Parse);
    }
    Ok(member)
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

struct BareItem {
    serialized: String,
    is_boolean_true: bool,
}

struct Number {
    serialized: String,
    is_decimal: bool,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
        }
    }

    fn parse_dictionary(
        &mut self,
        target_key: &str,
    ) -> Result<Option<String>, StructuredFieldError> {
        let mut selected = None;
        while !self.is_empty() {
            let key = self.parse_key()?;
            let member = if self.consume_if(b'=') {
                self.parse_item_or_inner_list()?
            } else {
                let parameters = self.parse_parameters()?;
                format!("?1{parameters}")
            };

            if key == target_key {
                selected = Some(member);
            }

            self.discard_ows();
            if self.is_empty() {
                return Ok(selected);
            }
            if !self.consume_if(b',') {
                return Err(StructuredFieldError::Parse);
            }
            self.discard_ows();
            if self.is_empty() {
                return Err(StructuredFieldError::Parse);
            }
        }
        Ok(selected)
    }

    fn parse_item_or_inner_list(&mut self) -> Result<String, StructuredFieldError> {
        if self.peek() == Some(b'(') {
            self.parse_inner_list()
        } else {
            self.parse_item()
        }
    }

    fn parse_inner_list(&mut self) -> Result<String, StructuredFieldError> {
        if !self.consume_if(b'(') {
            return Err(StructuredFieldError::Parse);
        }

        let mut out = String::from("(");
        let mut first = true;
        loop {
            self.discard_sp();
            match self.peek() {
                Some(b')') => {
                    self.consume();
                    out.push(')');
                    out.push_str(&self.parse_parameters()?);
                    return Ok(out);
                }
                Some(_) => {
                    if !first {
                        out.push(' ');
                    }
                    out.push_str(&self.parse_item()?);
                    first = false;
                    if !matches!(self.peek(), Some(b' ' | b')')) {
                        return Err(StructuredFieldError::Parse);
                    }
                }
                None => return Err(StructuredFieldError::Parse),
            }
        }
    }

    fn parse_item(&mut self) -> Result<String, StructuredFieldError> {
        let bare_item = self.parse_bare_item()?;
        let parameters = self.parse_parameters()?;
        Ok(format!("{}{parameters}", bare_item.serialized))
    }

    fn parse_parameters(&mut self) -> Result<String, StructuredFieldError> {
        let mut parameters = Vec::<(String, BareItem)>::new();
        while self.consume_if(b';') {
            self.discard_sp();
            let key = self.parse_key()?;
            let value = if self.consume_if(b'=') {
                self.parse_bare_item()?
            } else {
                BareItem::boolean(true)
            };

            if let Some(index) = parameters
                .iter()
                .position(|(existing_key, _)| existing_key == &key)
            {
                parameters[index].1 = value;
            } else {
                parameters.push((key, value));
            }
        }

        let mut out = String::new();
        for (key, value) in parameters {
            out.push(';');
            out.push_str(&key);
            if !value.is_boolean_true {
                out.push('=');
                out.push_str(&value.serialized);
            }
        }
        Ok(out)
    }

    fn parse_bare_item(&mut self) -> Result<BareItem, StructuredFieldError> {
        match self.peek() {
            Some(b'-' | b'0'..=b'9') => self.parse_number().map(BareItem::from_number),
            Some(b'"') => self.parse_string(),
            Some(b'A'..=b'Z' | b'a'..=b'z' | b'*') => self.parse_token(),
            Some(b':') => self.parse_byte_sequence(),
            Some(b'?') => self.parse_boolean(),
            Some(b'@') => self.parse_date(),
            Some(b'%') => self.parse_display_string(),
            _ => Err(StructuredFieldError::Parse),
        }
    }

    fn parse_number(&mut self) -> Result<Number, StructuredFieldError> {
        let negative = self.consume_if(b'-');
        let integer_start = self.pos;
        if !self.peek().is_some_and(|b| b.is_ascii_digit()) {
            return Err(StructuredFieldError::Parse);
        }
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.consume();
        }
        let integer_digits = &self.input[integer_start..self.pos];

        if self.consume_if(b'.') {
            if integer_digits.len() > 12 {
                return Err(StructuredFieldError::Parse);
            }

            let fraction_start = self.pos;
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.consume();
            }
            let fraction_digits = &self.input[fraction_start..self.pos];
            if fraction_digits.is_empty() || fraction_digits.len() > 3 {
                return Err(StructuredFieldError::Parse);
            }

            Ok(Number {
                serialized: serialize_decimal(negative, integer_digits, fraction_digits),
                is_decimal: true,
            })
        } else {
            if integer_digits.len() > 15 {
                return Err(StructuredFieldError::Parse);
            }

            Ok(Number {
                serialized: serialize_integer(negative, integer_digits),
                is_decimal: false,
            })
        }
    }

    fn parse_string(&mut self) -> Result<BareItem, StructuredFieldError> {
        if !self.consume_if(b'"') {
            return Err(StructuredFieldError::Parse);
        }

        let mut out = String::from("\"");
        loop {
            let Some(byte) = self.consume() else {
                return Err(StructuredFieldError::Parse);
            };
            match byte {
                b'\\' => {
                    let Some(next) = self.consume() else {
                        return Err(StructuredFieldError::Parse);
                    };
                    if !matches!(next, b'"' | b'\\') {
                        return Err(StructuredFieldError::Parse);
                    }
                    out.push('\\');
                    out.push(next as char);
                }
                b'"' => {
                    out.push('"');
                    return Ok(BareItem::new(out));
                }
                0x00..=0x1f | 0x7f => return Err(StructuredFieldError::Parse),
                _ => out.push(byte as char),
            }
        }
    }

    fn parse_token(&mut self) -> Result<BareItem, StructuredFieldError> {
        if !self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'*')
        {
            return Err(StructuredFieldError::Parse);
        }

        let start = self.pos;
        self.consume();
        while self
            .peek()
            .is_some_and(|byte| is_tchar(byte) || matches!(byte, b':' | b'/'))
        {
            self.consume();
        }
        Ok(BareItem::new(as_ascii_string(&self.input[start..self.pos])))
    }

    fn parse_byte_sequence(&mut self) -> Result<BareItem, StructuredFieldError> {
        if !self.consume_if(b':') {
            return Err(StructuredFieldError::Parse);
        }
        let start = self.pos;
        while !matches!(self.peek(), Some(b':') | None) {
            self.consume();
        }
        if !self.consume_if(b':') {
            return Err(StructuredFieldError::Parse);
        }

        let content = &self.input[start..self.pos - 1];
        if content.iter().any(|byte| !is_base64_char(*byte)) {
            return Err(StructuredFieldError::Parse);
        }

        let mut padded = as_ascii_string(content);
        for _ in 0..((4 - padded.len() % 4) % 4) {
            padded.push('=');
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(padded.as_bytes())
            .map_err(|_| StructuredFieldError::Parse)?;
        Ok(BareItem::new(format!(
            ":{}:",
            base64::engine::general_purpose::STANDARD.encode(decoded)
        )))
    }

    fn parse_boolean(&mut self) -> Result<BareItem, StructuredFieldError> {
        if !self.consume_if(b'?') {
            return Err(StructuredFieldError::Parse);
        }
        match self.consume() {
            Some(b'1') => Ok(BareItem::boolean(true)),
            Some(b'0') => Ok(BareItem::boolean(false)),
            _ => Err(StructuredFieldError::Parse),
        }
    }

    fn parse_date(&mut self) -> Result<BareItem, StructuredFieldError> {
        if !self.consume_if(b'@') {
            return Err(StructuredFieldError::Parse);
        }
        let number = self.parse_number()?;
        if number.is_decimal {
            return Err(StructuredFieldError::Parse);
        }
        Ok(BareItem::new(format!("@{}", number.serialized)))
    }

    fn parse_display_string(&mut self) -> Result<BareItem, StructuredFieldError> {
        if !self.consume_if(b'%') || !self.consume_if(b'"') {
            return Err(StructuredFieldError::Parse);
        }

        let mut bytes = Vec::new();
        loop {
            let Some(byte) = self.consume() else {
                return Err(StructuredFieldError::Parse);
            };
            if byte.is_ascii_control() || byte == 0x7f {
                return Err(StructuredFieldError::Parse);
            }
            match byte {
                b'%' => {
                    let high = self.consume().ok_or(StructuredFieldError::Parse)?;
                    let low = self.consume().ok_or(StructuredFieldError::Parse)?;
                    let high = lowercase_hex_value(high).ok_or(StructuredFieldError::Parse)?;
                    let low = lowercase_hex_value(low).ok_or(StructuredFieldError::Parse)?;
                    bytes.push((high << 4) | low);
                }
                b'"' => {
                    let value =
                        std::str::from_utf8(&bytes).map_err(|_| StructuredFieldError::Parse)?;
                    return Ok(BareItem::new(serialize_display_string(value)));
                }
                _ => bytes.push(byte),
            }
        }
    }

    fn parse_key(&mut self) -> Result<String, StructuredFieldError> {
        if !self
            .peek()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte == b'*')
        {
            return Err(StructuredFieldError::Parse);
        }

        let start = self.pos;
        self.consume();
        while self.peek().is_some_and(is_key_char) {
            self.consume();
        }
        Ok(as_ascii_string(&self.input[start..self.pos]))
    }

    fn discard_sp(&mut self) {
        while self.consume_if(b' ') {}
    }

    fn discard_ows(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.consume();
        }
    }

    fn consume_if(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn consume(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn is_empty(&self) -> bool {
        self.pos == self.input.len()
    }
}

impl BareItem {
    fn new(serialized: String) -> Self {
        Self {
            serialized,
            is_boolean_true: false,
        }
    }

    fn boolean(value: bool) -> Self {
        Self {
            serialized: if value { "?1" } else { "?0" }.to_owned(),
            is_boolean_true: value,
        }
    }

    fn from_number(number: Number) -> Self {
        Self::new(number.serialized)
    }
}

fn serialize_integer(negative: bool, digits: &[u8]) -> String {
    let digits = strip_leading_zeroes(digits);
    let mut out = String::new();
    if negative && digits != b"0" {
        out.push('-');
    }
    out.push_str(&as_ascii_string(digits));
    out
}

fn serialize_decimal(negative: bool, integer_digits: &[u8], fraction_digits: &[u8]) -> String {
    let integer_digits = strip_leading_zeroes(integer_digits);
    let fraction_digits = strip_trailing_zeroes(fraction_digits);
    let has_non_zero_digit = integer_digits != b"0" || fraction_digits.iter().any(|b| *b != b'0');

    let mut out = String::new();
    if negative && has_non_zero_digit {
        out.push('-');
    }
    out.push_str(&as_ascii_string(integer_digits));
    out.push('.');
    out.push_str(&as_ascii_string(fraction_digits));
    out
}

fn serialize_display_string(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut out = String::from("%\"");
    for byte in value.bytes() {
        if matches!(byte, b'%' | b'"' | 0x00..=0x1f | 0x7f..=0xff) {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        } else {
            out.push(byte as char);
        }
    }
    out.push('"');
    out
}

fn strip_leading_zeroes(mut digits: &[u8]) -> &[u8] {
    while digits.len() > 1 && digits.first() == Some(&b'0') {
        digits = &digits[1..];
    }
    if digits.is_empty() { b"0" } else { digits }
}

fn strip_trailing_zeroes(mut digits: &[u8]) -> &[u8] {
    while digits.len() > 1 && digits.last() == Some(&b'0') {
        digits = &digits[..digits.len() - 1];
    }
    if digits.is_empty() { b"0" } else { digits }
}

fn as_ascii_string(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| *byte as char).collect()
}

fn is_key_char(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.' | b'*')
}

fn is_tchar(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn is_base64_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')
}

fn lowercase_hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_and_serializes_dictionary_members() {
        let input = "a=1, b=2;x=1;y=2, c=(a   b    c), d";

        assert_eq!(dictionary_member(input, "a").unwrap(), Some("1".to_owned()));
        assert_eq!(
            dictionary_member(input, "b").unwrap(),
            Some("2;x=1;y=2".to_owned())
        );
        assert_eq!(
            dictionary_member(input, "c").unwrap(),
            Some("(a b c)".to_owned())
        );
        assert_eq!(
            dictionary_member(input, "d").unwrap(),
            Some("?1".to_owned())
        );
    }

    #[test]
    fn duplicate_keys_and_parameters_keep_last_value() {
        assert_eq!(
            dictionary_member("a=1, a=2", "a").unwrap(),
            Some("2".to_owned())
        );
        assert_eq!(
            dictionary_member("a=1;x=1;x=2", "a").unwrap(),
            Some("1;x=2".to_owned())
        );
    }

    #[test]
    fn duplicate_parameters_keep_original_position_and_last_value() {
        // RFC 9651 overwrites duplicate parameter values without moving the key.
        assert_eq!(
            dictionary_member("a=1;x=1;y=2;x=3", "a").unwrap(),
            Some("1;x=3;y=2".to_owned())
        );
    }

    #[test]
    fn canonicalizes_bare_items() {
        assert_eq!(
            dictionary_member("a=-000, b=01.230, c=:AQI:, d=@0001", "a").unwrap(),
            Some("0".to_owned())
        );
        assert_eq!(
            dictionary_member("a=-000, b=01.230, c=:AQI:, d=@0001", "b").unwrap(),
            Some("1.23".to_owned())
        );
        assert_eq!(
            dictionary_member("a=-000, b=01.230, c=:AQI:, d=@0001", "c").unwrap(),
            Some(":AQI=:".to_owned())
        );
        assert_eq!(
            dictionary_member("a=-000, b=01.230, c=:AQI:, d=@0001", "d").unwrap(),
            Some("@1".to_owned())
        );
    }
}
