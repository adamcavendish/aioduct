mod ast;
mod parser;

#[cfg(test)]
mod tests;

pub(crate) use parser::{dictionary, dictionary_member, field_value, serialize_dictionary};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StructuredFieldError {
    Parse,
}
