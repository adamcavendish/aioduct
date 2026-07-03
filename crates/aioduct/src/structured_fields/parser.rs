use base64::Engine as _;

use super::StructuredFieldError;
use super::ast::{
    BareItem, Dictionary, InnerList, Item, ItemOrInnerList, List, Parameters, StructuredFieldType,
    StructuredFieldValue, serialize_decimal, serialize_dictionary_member, serialize_integer,
};

struct Number {
    bare_item: BareItem,
    is_decimal: bool,
}

pub(crate) fn parse_field_value(
    input: &str,
    field_type: StructuredFieldType,
) -> Result<StructuredFieldValue, StructuredFieldError> {
    match field_type {
        StructuredFieldType::Dictionary => {
            parse_dictionary_field(input).map(StructuredFieldValue::Dictionary)
        }
        StructuredFieldType::List => parse_list_field(input).map(StructuredFieldValue::List),
        StructuredFieldType::Item => parse_item_field(input).map(StructuredFieldValue::Item),
    }
}

pub(crate) fn parse_dictionary_field(input: &str) -> Result<Dictionary, StructuredFieldError> {
    if !input.is_ascii() {
        return Err(StructuredFieldError::Parse);
    }

    Parser::new(input).parse_complete(|parser| parser.parse_dictionary_field())
}

pub(crate) fn parse_list_field(input: &str) -> Result<List, StructuredFieldError> {
    if !input.is_ascii() {
        return Err(StructuredFieldError::Parse);
    }

    Parser::new(input).parse_complete(|parser| parser.parse_list_field())
}

pub(crate) fn parse_item_field(input: &str) -> Result<Item, StructuredFieldError> {
    if !input.is_ascii() {
        return Err(StructuredFieldError::Parse);
    }

    Parser::new(input).parse_complete(|parser| parser.parse_item())
}

#[cfg(test)]
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

#[cfg(test)]
pub(crate) fn field_value(input: &str) -> Result<String, StructuredFieldError> {
    parse_field_value(input, StructuredFieldType::Dictionary)
        .or_else(|_| parse_field_value(input, StructuredFieldType::List))
        .or_else(|_| parse_field_value(input, StructuredFieldType::Item))
        .map(|value| value.serialize())
}

pub(crate) fn dictionary(input: &str) -> Result<Vec<(String, String)>, StructuredFieldError> {
    if !input.is_ascii() {
        return Err(StructuredFieldError::Parse);
    }

    Parser::new(input)
        .parse_complete(|parser| parser.parse_dictionary_entries())
        .map(|entries| {
            entries
                .into_iter()
                .map(|(key, member)| (key, member.serialize()))
                .collect()
        })
}

pub(crate) fn serialize_dictionary(entries: &[(String, String)]) -> String {
    let mut out = String::new();
    for (index, (key, member)) in entries.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&serialize_dictionary_member(key, member));
    }
    out
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
        }
    }

    #[cfg(test)]
    fn parse_dictionary(
        &mut self,
        target_key: &str,
    ) -> Result<Option<String>, StructuredFieldError> {
        let mut selected = None;
        for (key, member) in self.parse_dictionary_entries()? {
            if key == target_key {
                selected = Some(member.serialize());
            }
        }
        Ok(selected)
    }

    fn parse_dictionary_field(&mut self) -> Result<Dictionary, StructuredFieldError> {
        let mut entries = Vec::<(String, ItemOrInnerList)>::new();
        for (key, member) in self.parse_dictionary_entries()? {
            replace_or_append(&mut entries, key, member);
        }
        Ok(Dictionary { entries })
    }

    fn parse_dictionary_entries(
        &mut self,
    ) -> Result<Vec<(String, ItemOrInnerList)>, StructuredFieldError> {
        let mut members = Vec::<(String, ItemOrInnerList)>::new();
        while !self.is_empty() {
            let key = self.parse_key()?;
            let member = if self.consume_if(b'=') {
                self.parse_item_or_inner_list()?
            } else {
                let parameters = self.parse_parameters()?;
                ItemOrInnerList::Item(Item {
                    bare_item: BareItem::Boolean(true),
                    parameters,
                })
            };

            members.push((key, member));

            self.discard_ows();
            if self.is_empty() {
                return Ok(members);
            }
            if !self.consume_if(b',') {
                return Err(StructuredFieldError::Parse);
            }
            self.discard_ows();
            if self.is_empty() {
                return Err(StructuredFieldError::Parse);
            }
        }
        Ok(members)
    }

    fn parse_list_field(&mut self) -> Result<List, StructuredFieldError> {
        let mut members = Vec::new();
        while !self.is_empty() {
            members.push(self.parse_item_or_inner_list()?);
            self.discard_ows();
            if self.is_empty() {
                return Ok(List { members });
            }
            if !self.consume_if(b',') {
                return Err(StructuredFieldError::Parse);
            }
            self.discard_ows();
            if self.is_empty() {
                return Err(StructuredFieldError::Parse);
            }
        }
        Ok(List { members })
    }

    fn parse_item_or_inner_list(&mut self) -> Result<ItemOrInnerList, StructuredFieldError> {
        if self.peek() == Some(b'(') {
            self.parse_inner_list().map(ItemOrInnerList::InnerList)
        } else {
            self.parse_item().map(ItemOrInnerList::Item)
        }
    }

    fn parse_inner_list(&mut self) -> Result<InnerList, StructuredFieldError> {
        if !self.consume_if(b'(') {
            return Err(StructuredFieldError::Parse);
        }

        let mut items = Vec::new();
        loop {
            self.discard_sp();
            match self.peek() {
                Some(b')') => {
                    self.consume();
                    let parameters = self.parse_parameters()?;
                    return Ok(InnerList { items, parameters });
                }
                Some(_) => {
                    items.push(self.parse_item()?);
                    if !matches!(self.peek(), Some(b' ' | b')')) {
                        return Err(StructuredFieldError::Parse);
                    }
                }
                None => return Err(StructuredFieldError::Parse),
            }
        }
    }

    fn parse_item(&mut self) -> Result<Item, StructuredFieldError> {
        let bare_item = self.parse_bare_item()?;
        let parameters = self.parse_parameters()?;
        Ok(Item {
            bare_item,
            parameters,
        })
    }

    fn parse_parameters(&mut self) -> Result<Parameters, StructuredFieldError> {
        let mut entries = Vec::<(String, BareItem)>::new();
        while self.consume_if(b';') {
            self.discard_sp();
            let key = self.parse_key()?;
            let value = if self.consume_if(b'=') {
                self.parse_bare_item()?
            } else {
                BareItem::Boolean(true)
            };

            replace_or_append(&mut entries, key, value);
        }

        Ok(Parameters { entries })
    }

    fn parse_bare_item(&mut self) -> Result<BareItem, StructuredFieldError> {
        match self.peek() {
            Some(b'-' | b'0'..=b'9') => self.parse_number().map(|number| number.bare_item),
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
                bare_item: BareItem::Decimal(serialize_decimal(
                    negative,
                    integer_digits,
                    fraction_digits,
                )),
                is_decimal: true,
            })
        } else {
            if integer_digits.len() > 15 {
                return Err(StructuredFieldError::Parse);
            }

            Ok(Number {
                bare_item: BareItem::Integer(serialize_integer(negative, integer_digits)),
                is_decimal: false,
            })
        }
    }

    fn parse_string(&mut self) -> Result<BareItem, StructuredFieldError> {
        if !self.consume_if(b'"') {
            return Err(StructuredFieldError::Parse);
        }

        let mut out = String::new();
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
                    out.push(next as char);
                }
                b'"' => return Ok(BareItem::String(out)),
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
        Ok(BareItem::Token(as_ascii_string(
            &self.input[start..self.pos],
        )))
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
        Ok(BareItem::ByteSequence(decoded))
    }

    fn parse_boolean(&mut self) -> Result<BareItem, StructuredFieldError> {
        if !self.consume_if(b'?') {
            return Err(StructuredFieldError::Parse);
        }
        match self.consume() {
            Some(b'1') => Ok(BareItem::Boolean(true)),
            Some(b'0') => Ok(BareItem::Boolean(false)),
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
        match number.bare_item {
            BareItem::Integer(value) => Ok(BareItem::Date(format!("@{value}"))),
            _ => Err(StructuredFieldError::Parse),
        }
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
                    return Ok(BareItem::DisplayString(value.to_owned()));
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

    fn parse_complete<T>(
        mut self,
        parse: impl FnOnce(&mut Self) -> Result<T, StructuredFieldError>,
    ) -> Result<T, StructuredFieldError> {
        self.discard_sp();
        let value = parse(&mut self)?;
        self.discard_sp();
        if !self.is_empty() {
            return Err(StructuredFieldError::Parse);
        }
        Ok(value)
    }
}

fn replace_or_append<T>(entries: &mut Vec<(String, T)>, key: String, member: T) {
    if let Some(index) = entries
        .iter()
        .position(|(existing_key, _)| existing_key == &key)
    {
        entries[index].1 = member;
    } else {
        entries.push((key, member));
    }
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
