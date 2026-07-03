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
fn serializes_complete_structured_field_values() {
    assert_eq!(
        field_value(" a=1,    b=2;x=1;y=2,   c=(a   b   c), d ").unwrap(),
        "a=1, b=2;x=1;y=2, c=(a b c), d"
    );
    assert_eq!(
        field_value(" 1, 02.300, (a   b);q=01.200 ").unwrap(),
        "1, 2.3, (a b);q=1.2"
    );
    assert_eq!(
        field_value(" \"hello\\\"\";a=1;a=2 ").unwrap(),
        "\"hello\\\"\";a=2"
    );
}

#[test]
fn duplicate_dictionary_keys_keep_original_position_and_last_value() {
    assert_eq!(field_value("a=1, b=2, a=3").unwrap(), "a=3, b=2");
}

#[test]
fn dictionary_preserves_duplicate_entries_for_callers() {
    assert_eq!(
        dictionary("a=1, b=2, a=3").unwrap(),
        vec![
            ("a".to_owned(), "1".to_owned()),
            ("b".to_owned(), "2".to_owned()),
            ("a".to_owned(), "3".to_owned()),
        ]
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

#[test]
fn canonicalizes_lists_items_inner_lists_and_parameters() {
    assert_eq!(
        field_value("  01;foo;bar=?0, 02.300, (  a;z=01 b   c;x;x=?0  );q=01.200  ").unwrap(),
        "1;foo;bar=?0, 2.3, (a;z=1 b c;x=?0);q=1.2"
    );
    assert_eq!(
        field_value(r#" "hello\"";a=1;a=2 "#).unwrap(),
        r#""hello\"";a=2"#
    );
}

#[test]
fn canonicalizes_dictionary_boolean_members_and_duplicates() {
    assert_eq!(
        field_value("a, b=?0, c=?1, a=?0, d;x;y=?0").unwrap(),
        "a=?0, b=?0, c, d;x;y=?0"
    );
    assert_eq!(
        dictionary("a, b=?0, a=?1").unwrap(),
        vec![
            ("a".to_owned(), "?1".to_owned()),
            ("b".to_owned(), "?0".to_owned()),
            ("a".to_owned(), "?1".to_owned()),
        ]
    );
}

#[test]
fn canonicalizes_number_boundaries() {
    assert_eq!(
        field_value("a=999999999999999, b=-999999999999999, c=999999999999.999").unwrap(),
        "a=999999999999999, b=-999999999999999, c=999999999999.999"
    );
    assert_eq!(
        field_value("a=000000000000000, b=-000000000000000, c=000000000000.010").unwrap(),
        "a=0, b=0, c=0.01"
    );
}

#[test]
fn rejects_malformed_numbers() {
    for input in [
        "a=1000000000000000",
        "a=1000000000000.0",
        "a=1.",
        "a=1.0000",
        "a=-",
        "a=--1",
    ] {
        assert!(field_value(input).is_err(), "{input}");
    }
}

#[test]
fn canonicalizes_byte_sequences_dates_and_display_strings() {
    assert_eq!(
        field_value(r#"a=:AQI:, b=:AQI=:"#).unwrap(),
        "a=:AQI=:, b=:AQI=:"
    );
    assert_eq!(field_value("a=@000001").unwrap(), "a=@1");
    assert_eq!(
        field_value(r#"a=%"hello%20%22%25", b=%"caf%c3%a9""#).unwrap(),
        r#"a=%"hello %22%25", b=%"caf%c3%a9""#
    );
}

#[test]
fn rejects_malformed_byte_sequences_dates_and_display_strings() {
    for input in [
        "a=:AQI",
        "a=:AQI!:",
        "a=@1.0",
        "a=@",
        r#"a=%"bad%C3%A9""#,
        r#"a=%"bad%ff""#,
        r#"a=%"bad%g0""#,
        "a=%\"bad\u{7f}\"",
    ] {
        assert!(field_value(input).is_err(), "{input}");
    }
}

#[test]
fn documents_current_top_level_inference_order() {
    assert_eq!(field_value("a=1").unwrap(), "a=1");
    assert_eq!(field_value("a;b=1").unwrap(), "a;b=1");
    assert_eq!(field_value("token/with:path").unwrap(), "token/with:path");
}
