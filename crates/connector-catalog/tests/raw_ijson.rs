use std::path::Path;

use donat_connector_catalog::canonicalize_raw;

fn fixture_bytes(name: &str) -> Vec<u8> {
    let bytes = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/canonical")
            .join(name),
    )
    .unwrap();
    if name != "invalid-utf8.json" {
        return bytes;
    }
    let hex = std::str::from_utf8(&bytes).unwrap().trim();
    assert_eq!(hex.len() % 2, 0, "hex fixture must contain whole bytes");
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(pair, 16).unwrap()
        })
        .collect()
}

fn assert_raw_error(name: &str, expected: &str) {
    assert_eq!(
        canonicalize_raw(&fixture_bytes(name)).unwrap_err().code(),
        expected
    );
}

#[test]
fn raw_ijson_rejections_are_exact() {
    assert_raw_error(
        "escaped-noncharacter.json",
        "catalog_jcs_disallowed_unicode",
    );
    assert_raw_error(
        "unescaped-noncharacter.json",
        "catalog_jcs_disallowed_unicode",
    );
    assert_raw_error("lone-surrogate.json", "catalog_jcs_invalid_surrogate");
    assert_raw_error(
        "invalid-surrogate-pair.json",
        "catalog_jcs_invalid_surrogate",
    );
    assert_raw_error("invalid-utf8.json", "catalog_jcs_invalid_utf8");
    assert_raw_error(
        "duplicate-decoded-name.json",
        "catalog_jcs_duplicate_member",
    );
    assert_raw_error(
        "number-outside-binary64.json",
        "canonical_json_number_not_exact",
    );
    assert_raw_error("number-non-finite.json", "canonical_json_number_not_exact");
    assert_eq!(
        canonicalize_raw(&fixture_bytes("number-exact-binary64.json")).unwrap(),
        b"9007199254740992",
    );
    let mut expected = fixture_bytes("recursive-utf16-order.expected.json");
    if expected.last() == Some(&b'\n') {
        expected.pop();
    }
    assert_eq!(
        canonicalize_raw(&fixture_bytes("recursive-utf16-order.json")).unwrap(),
        expected,
    );
}
