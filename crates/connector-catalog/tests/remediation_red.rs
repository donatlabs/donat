use std::path::Path;
use std::process::Command;

use donat_connector_catalog::canonicalize_raw;

#[test]
fn raw_numbers_use_exact_ecmascript_canonicalization() {
    let accepted: &[(&[u8], &[u8])] = &[
        (b"1.0", b"1"),
        (b"-0.0", b"0"),
        (b"0.000001", b"0.000001"),
        (b"1e-6", b"0.000001"),
        (b"1e-7", b"1e-7"),
        (b"100000000000000000000", b"100000000000000000000"),
        (b"1e20", b"100000000000000000000"),
        (b"1e21", b"1e+21"),
        (b"5e-324", b"5e-324"),
        (b"1.7976931348623157e308", b"1.7976931348623157e+308"),
        (b"9007199254740992", b"9007199254740992"),
    ];
    for (raw, expected) in accepted {
        assert_eq!(
            canonicalize_raw(raw).unwrap(),
            *expected,
            "raw number {}",
            String::from_utf8_lossy(raw)
        );
    }

    for raw in [
        b"0.10000000000000001".as_slice(),
        b"18446744073709551615",
        b"9007199254740993",
        b"1e400",
        b"1e-400",
        b"9223372036854775807",
        b"9223372036854775808",
    ] {
        assert_eq!(
            canonicalize_raw(raw).unwrap_err().code(),
            "canonical_json_number_not_exact",
            "raw number {}",
            String::from_utf8_lossy(raw)
        );
    }
}

#[test]
fn every_finite_rfc_8785_appendix_b_vector_is_exact() {
    // Independent constants from RFC 8785 Appendix B. Non-finite rows are
    // intentionally absent because I-JSON rejects them.
    let vectors = [
        (0x0000_0000_0000_0000, "0"),
        (0x8000_0000_0000_0000, "0"),
        (0x0000_0000_0000_0001, "5e-324"),
        (0x8000_0000_0000_0001, "-5e-324"),
        (0x7fef_ffff_ffff_ffff, "1.7976931348623157e+308"),
        (0xffef_ffff_ffff_ffff, "-1.7976931348623157e+308"),
        (0x4340_0000_0000_0000, "9007199254740992"),
        (0xc340_0000_0000_0000, "-9007199254740992"),
        (0x4430_0000_0000_0000, "295147905179352830000"),
        (0x44b5_2d02_c7e1_4af5, "9.999999999999997e+22"),
        (0x44b5_2d02_c7e1_4af6, "1e+23"),
        (0x44b5_2d02_c7e1_4af7, "1.0000000000000001e+23"),
        (0x444b_1ae4_d6e2_ef4e, "999999999999999700000"),
        (0x444b_1ae4_d6e2_ef4f, "999999999999999900000"),
        (0x444b_1ae4_d6e2_ef50, "1e+21"),
        (0x3eb0_c6f7_a0b5_ed8c, "9.999999999999997e-7"),
        (0x3eb0_c6f7_a0b5_ed8d, "0.000001"),
        (0x41b3_de43_5555_5553, "333333333.3333332"),
        (0x41b3_de43_5555_5554, "333333333.33333325"),
        (0x41b3_de43_5555_5555, "333333333.3333333"),
        (0x41b3_de43_5555_5556, "333333333.3333334"),
        (0x41b3_de43_5555_5557, "333333333.33333343"),
        (0xbecb_f647_612f_3696, "-0.0000033333333333333333"),
        (0x4314_3ff3_c1cb_0959, "1424953923781206.2"),
    ];

    for (bits, expected) in vectors {
        let value = f64::from_bits(bits);
        let mut buffer = ryu_js::Buffer::new();
        assert_eq!(buffer.format_finite(value), expected, "{bits:016x}");
        assert_eq!(
            canonicalize_raw(expected.as_bytes()).unwrap(),
            expected.as_bytes(),
            "{bits:016x}"
        );
    }
}

#[test]
fn raw_number_cursor_ignores_strings_and_covers_nested_values() {
    assert_eq!(
        canonicalize_raw(
            br#"{"z":[1.0,{"n":1e-7,"s":"1.0","escaped":"\"9007199254740993\""}],"a":-0.0}"#
        )
        .unwrap(),
        br#"{"a":0,"z":[1,{"escaped":"\"9007199254740993\"","n":1e-7,"s":"1.0"}]}"#
    );
    for malformed in [
        b"1x".as_slice(),
        b"1e+",
        b"9007199254740993x",
        b"[9007199254740993,]",
    ] {
        assert_eq!(
            canonicalize_raw(malformed).unwrap_err().code(),
            "catalog_jcs_schema_mismatch"
        );
    }
}

#[test]
fn checked_in_donat_http_source_identity_is_exact() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("catalog crate is nested below workspace root");
    let status = Command::new("python3")
        .arg(workspace.join("scripts/check_connector_source_identity.py"))
        .current_dir(workspace)
        .status()
        .expect("repository source-identity checker must exist");
    assert!(status.success(), "checked-in source identity must verify");
}

#[test]
fn source_identity_checker_pins_the_complete_legal_and_notice_disposition() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("catalog crate is nested below workspace root");
    let checker =
        std::fs::read_to_string(workspace.join("scripts/check_connector_source_identity.py"))
            .unwrap();
    for (field, value) in [
        ("spdx_id", "Apache-2.0"),
        ("selected_dual_license_branch", "null"),
        ("notice_id", "notice.donat.http.v1"),
        ("required_copyright_lines", "[]"),
        ("notice_bundle_destination", "THIRD_PARTY_NOTICES.md"),
    ] {
        assert!(
            checker.contains(&format!("\"{field}\": \"{value}\"")),
            "checker must pin {field}={value}"
        );
    }
}
