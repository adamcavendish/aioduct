use std::collections::HashSet;

use http::header::{HeaderMap, HeaderName, HeaderValue};

use super::MessageSignatureError;
use super::structured_fields;

pub(crate) const SIGNATURE_INPUT: &str = "signature-input";
pub(crate) const SIGNATURE: &str = "signature";

/// Generated `Signature-Input` and `Signature` header values.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct MessageSignatureHeaders {
    pub(crate) label: String,
    /// The `Signature-Input` header value.
    pub signature_input: HeaderValue,
    /// The `Signature` header value.
    pub signature: HeaderValue,
}

impl MessageSignatureHeaders {
    /// Merge the generated label into a header map.
    pub fn insert_into(self, headers: &mut HeaderMap) -> Result<(), MessageSignatureError> {
        let generated_input = single_generated_member(
            SIGNATURE_INPUT,
            self.signature_input
                .to_str()
                .map_err(|_| MessageSignatureError::MalformedSignatureHeader(SIGNATURE_INPUT))?,
            &self.label,
        )?;
        let generated_signature = single_generated_member(
            SIGNATURE,
            self.signature
                .to_str()
                .map_err(|_| MessageSignatureError::MalformedSignatureHeader(SIGNATURE))?,
            &self.label,
        )?;

        let mut signature_input = existing_dictionary(headers, SIGNATURE_INPUT)?;
        let mut signature = existing_dictionary(headers, SIGNATURE)?;

        remove_member(&mut signature_input, &self.label);
        remove_member(&mut signature, &self.label);
        reject_duplicate_labels(SIGNATURE_INPUT, &signature_input)?;
        reject_duplicate_labels(SIGNATURE, &signature)?;
        ensure_matching_labels(&signature_input, &signature)?;

        merge_member(&mut signature_input, self.label.clone(), generated_input);
        merge_member(&mut signature, self.label, generated_signature);

        insert_dictionary(headers, SIGNATURE_INPUT, &signature_input)?;
        insert_dictionary(headers, SIGNATURE, &signature)?;
        Ok(())
    }
}

pub(crate) fn remove_label(
    headers: &mut HeaderMap,
    label: &str,
) -> Result<(), MessageSignatureError> {
    let mut signature_input = existing_dictionary(headers, SIGNATURE_INPUT)?;
    let mut signature = existing_dictionary(headers, SIGNATURE)?;

    remove_member(&mut signature_input, label);
    remove_member(&mut signature, label);
    reject_duplicate_labels(SIGNATURE_INPUT, &signature_input)?;
    reject_duplicate_labels(SIGNATURE, &signature)?;
    ensure_matching_labels(&signature_input, &signature)?;

    set_dictionary(headers, SIGNATURE_INPUT, &signature_input)?;
    set_dictionary(headers, SIGNATURE, &signature)?;
    Ok(())
}

pub(crate) fn existing_dictionary(
    headers: &HeaderMap,
    name: &'static str,
) -> Result<Vec<(String, String)>, MessageSignatureError> {
    let Some(value) = combined_header_value(headers, name)? else {
        return Ok(Vec::new());
    };
    let entries = structured_fields::dictionary(&value)
        .map_err(|_| MessageSignatureError::MalformedSignatureHeader(name))?;
    Ok(entries)
}

fn combined_header_value(
    headers: &HeaderMap,
    name: &'static str,
) -> Result<Option<String>, MessageSignatureError> {
    let values = headers.get_all(HeaderName::from_static(name));
    let mut out = Vec::new();
    for value in values {
        let value = value
            .to_str()
            .map_err(|_| MessageSignatureError::MalformedSignatureHeader(name))?;
        out.push(value.trim_matches(|c| c == ' ' || c == '\t').to_owned());
    }
    if out.is_empty() {
        Ok(None)
    } else {
        Ok(Some(out.join(", ")))
    }
}

fn single_generated_member(
    header: &'static str,
    value: &str,
    label: &str,
) -> Result<String, MessageSignatureError> {
    let entries = structured_fields::dictionary(value)
        .map_err(|_| MessageSignatureError::MalformedSignatureHeader(header))?;
    match entries.as_slice() {
        [(entry_label, member)] if entry_label == label => Ok(member.clone()),
        _ => Err(MessageSignatureError::MalformedSignatureHeader(header)),
    }
}

pub(crate) fn reject_duplicate_labels(
    header: &'static str,
    entries: &[(String, String)],
) -> Result<(), MessageSignatureError> {
    let mut seen = HashSet::new();
    for (label, _) in entries {
        if !seen.insert(label.as_str()) {
            return Err(MessageSignatureError::DuplicateSignatureLabel {
                header,
                label: label.clone(),
            });
        }
    }
    Ok(())
}

pub(crate) fn ensure_matching_labels(
    signature_input: &[(String, String)],
    signature: &[(String, String)],
) -> Result<(), MessageSignatureError> {
    if signature_input.is_empty() && signature.is_empty() {
        return Ok(());
    }
    if signature_input.len() != signature.len() {
        return Err(MessageSignatureError::MismatchedSignatureLabels);
    }
    let signature_labels = signature
        .iter()
        .map(|(label, _)| label.as_str())
        .collect::<HashSet<_>>();
    if signature_input
        .iter()
        .all(|(label, _)| signature_labels.contains(label.as_str()))
    {
        Ok(())
    } else {
        Err(MessageSignatureError::MismatchedSignatureLabels)
    }
}

fn merge_member(entries: &mut Vec<(String, String)>, label: String, member: String) {
    if let Some(index) = entries
        .iter()
        .position(|(existing_label, _)| existing_label == &label)
    {
        entries[index].1 = member;
    } else {
        entries.push((label, member));
    }
}

fn remove_member(entries: &mut Vec<(String, String)>, label: &str) {
    entries.retain(|(existing_label, _)| existing_label != label);
}

fn insert_dictionary(
    headers: &mut HeaderMap,
    name: &'static str,
    entries: &[(String, String)],
) -> Result<(), MessageSignatureError> {
    let value = structured_fields::serialize_dictionary(entries);
    headers.insert(
        HeaderName::from_static(name),
        HeaderValue::from_str(&value).map_err(|source| {
            MessageSignatureError::InvalidGeneratedHeader {
                header: header_display_name(name),
                source,
            }
        })?,
    );
    Ok(())
}

fn set_dictionary(
    headers: &mut HeaderMap,
    name: &'static str,
    entries: &[(String, String)],
) -> Result<(), MessageSignatureError> {
    if entries.is_empty() {
        headers.remove(HeaderName::from_static(name));
        Ok(())
    } else {
        insert_dictionary(headers, name, entries)
    }
}

fn header_display_name(name: &'static str) -> &'static str {
    match name {
        SIGNATURE_INPUT => "Signature-Input",
        SIGNATURE => "Signature",
        _ => name,
    }
}
