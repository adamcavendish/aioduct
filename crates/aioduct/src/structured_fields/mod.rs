mod ast;
mod parser;

#[cfg(test)]
mod tests;

pub(crate) use ast::{Dictionary, StructuredFieldType, StructuredFieldValue};
pub(crate) use parser::{
    dictionary, parse_dictionary_field, parse_field_value, serialize_dictionary,
};
#[cfg(test)]
pub(crate) use parser::{dictionary_member, field_value};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StructuredFieldError {
    Parse,
}
