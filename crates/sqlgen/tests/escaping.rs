//! Unit tests for the SQL escaping primitives `quote_lit` and `quote_ident`,
//! which are the engine's only defence against SQL injection (literals are
//! inlined, not bound as parameters — see crates/sqlgen/src/lib.rs header).
//! Every user-controlled value reaches SQL through `quote_lit`; every
//! identifier through `quote_ident`.

use donat_sqlgen::{quote_ident, quote_lit};

/// A literal is wrapped in single quotes with every embedded `'` doubled, and
/// nothing else is altered (Postgres `standard_conforming_strings = on`, so a
/// backslash is an ordinary character and needs no escaping).
#[test]
fn quote_lit_doubles_single_quotes() {
    assert_eq!(quote_lit("abc"), "'abc'");
    assert_eq!(quote_lit(""), "''");
    assert_eq!(quote_lit("O'Brien"), "'O''Brien'");
    // Already-doubled input is doubled again (correct: it is just data).
    assert_eq!(quote_lit("''"), "''''''");
}

#[test]
fn quote_lit_neutralises_classic_injection_payloads() {
    let cases = [
        // (raw payload, the doubled body that must appear inside the quotes)
        ("' OR '1'='1", "'' OR ''1''=''1"),
        ("'; DROP TABLE users; --", "''; DROP TABLE users; --"),
        ("admin'--", "admin''--"),
        ("' UNION SELECT password FROM users --", "'' UNION SELECT password FROM users --"),
        ("1'); DROP TABLE t; --", "1''); DROP TABLE t; --"),
        ("x' OR 1=1 --", "x'' OR 1=1 --"),
    ];
    for (raw, doubled) in cases {
        let out = quote_lit(raw);
        // Wrapped and every single quote doubled.
        assert_eq!(out, format!("'{doubled}'"), "wrong escaping for {raw:?}");
        // The escaped result has an even number of single quotes — no quote is
        // left to break out of the literal.
        assert_eq!(out.matches('\'').count() % 2, 0, "odd quote count for {raw:?}: {out}");
    }
}

#[test]
fn quote_lit_leaves_backslash_and_other_chars_literal() {
    // Backslash is inert under standard_conforming_strings; it must NOT be
    // turned into an escape sequence.
    assert_eq!(quote_lit("a\\b"), "'a\\b'");
    assert_eq!(quote_lit("\\'"), "'\\'''");
    // Dollar-quoting characters are just data.
    assert_eq!(quote_lit("$$"), "'$$'");
    // Newlines and control characters are preserved verbatim, not stripped.
    assert_eq!(quote_lit("a\nb"), "'a\nb'");
    assert_eq!(quote_lit("a\tb"), "'a\tb'");
}

/// An identifier is wrapped in double quotes with every embedded `"` doubled.
#[test]
fn quote_ident_doubles_double_quotes() {
    assert_eq!(quote_ident("col"), "\"col\"");
    assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
    // An identifier-injection attempt cannot break out of the quoting.
    assert_eq!(
        quote_ident("x\"; DROP TABLE t; --"),
        "\"x\"\"; DROP TABLE t; --\""
    );
    let out = quote_ident("a\"b\"c");
    assert_eq!(out.matches('"').count() % 2, 0, "odd double-quote count: {out}");
}

#[test]
fn quote_ident_and_quote_lit_do_not_cross_contaminate() {
    // A single quote is data to an identifier; a double quote is data to a
    // literal. Each helper only neutralises its own delimiter.
    assert_eq!(quote_ident("a'b"), "\"a'b\"");
    assert_eq!(quote_lit("a\"b"), "'a\"b'");
}
