use base64::Engine as _;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StructuredFieldType {
    Dictionary,
    List,
    Item,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum StructuredFieldValue {
    Dictionary(Dictionary),
    List(List),
    Item(Item),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Dictionary {
    pub(super) entries: Vec<(String, ItemOrInnerList)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct List {
    pub(super) members: Vec<ItemOrInnerList>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ItemOrInnerList {
    Item(Item),
    InnerList(InnerList),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InnerList {
    pub(super) items: Vec<Item>,
    pub(super) parameters: Parameters,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Item {
    pub(super) bare_item: BareItem,
    pub(super) parameters: Parameters,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Parameters {
    pub(super) entries: Vec<(String, BareItem)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BareItem {
    Integer(String),
    Decimal(String),
    String(String),
    Token(String),
    ByteSequence(Vec<u8>),
    Boolean(bool),
    Date(String),
    DisplayString(String),
}

impl StructuredFieldValue {
    pub(crate) fn serialize(&self) -> String {
        match self {
            Self::Dictionary(value) => value.serialize(),
            Self::List(value) => value.serialize(),
            Self::Item(value) => value.serialize(),
        }
    }
}

impl Dictionary {
    pub(crate) fn member(&self, key: &str) -> Option<&ItemOrInnerList> {
        self.entries
            .iter()
            .find(|(member_key, _)| member_key == key)
            .map(|(_, member)| member)
    }

    pub(crate) fn serialize(&self) -> String {
        let mut out = String::new();
        for (index, (key, member)) in self.entries.iter().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            out.push_str(&serialize_dictionary_member(key, &member.serialize()));
        }
        out
    }
}

impl List {
    pub(crate) fn serialize(&self) -> String {
        self.members
            .iter()
            .map(ItemOrInnerList::serialize)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl ItemOrInnerList {
    pub(crate) fn serialize(&self) -> String {
        match self {
            Self::Item(item) => item.serialize(),
            Self::InnerList(inner_list) => inner_list.serialize(),
        }
    }
}

impl InnerList {
    pub(crate) fn serialize(&self) -> String {
        let items = self
            .items
            .iter()
            .map(Item::serialize)
            .collect::<Vec<_>>()
            .join(" ");
        format!("({}){}", items, self.parameters.serialize())
    }
}

impl Item {
    pub(crate) fn serialize(&self) -> String {
        format!(
            "{}{}",
            self.bare_item.serialize(),
            self.parameters.serialize()
        )
    }
}

impl Parameters {
    pub(crate) fn serialize(&self) -> String {
        let mut out = String::new();
        for (key, value) in &self.entries {
            out.push(';');
            out.push_str(key);
            if !value.is_boolean_true() {
                out.push('=');
                out.push_str(&value.serialize());
            }
        }
        out
    }
}

impl BareItem {
    pub(crate) fn serialize(&self) -> String {
        match self {
            Self::Integer(value)
            | Self::Decimal(value)
            | Self::Token(value)
            | Self::Date(value) => value.clone(),
            Self::String(value) => serialize_string(value),
            Self::ByteSequence(value) => {
                format!(
                    ":{}:",
                    base64::engine::general_purpose::STANDARD.encode(value)
                )
            }
            Self::Boolean(true) => "?1".to_owned(),
            Self::Boolean(false) => "?0".to_owned(),
            Self::DisplayString(value) => serialize_display_string(value),
        }
    }

    pub(crate) fn is_boolean_true(&self) -> bool {
        matches!(self, Self::Boolean(true))
    }
}

pub(super) fn serialize_integer(negative: bool, digits: &[u8]) -> String {
    let digits = strip_leading_zeroes(digits);
    let mut out = String::new();
    if negative && digits != b"0" {
        out.push('-');
    }
    out.push_str(&as_ascii_string(digits));
    out
}

pub(super) fn serialize_decimal(
    negative: bool,
    integer_digits: &[u8],
    fraction_digits: &[u8],
) -> String {
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

fn serialize_string(value: &str) -> String {
    let mut out = String::from("\"");
    for byte in value.bytes() {
        match byte {
            b'"' | b'\\' => {
                out.push('\\');
                out.push(byte as char);
            }
            _ => out.push(byte as char),
        }
    }
    out.push('"');
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

pub(super) fn serialize_dictionary_member(key: &str, member: &str) -> String {
    let mut out = String::from(key);
    match member.strip_prefix("?1") {
        Some(parameters) if parameters.is_empty() || parameters.starts_with(';') => {
            out.push_str(parameters);
        }
        _ => {
            out.push('=');
            out.push_str(member);
        }
    }
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
