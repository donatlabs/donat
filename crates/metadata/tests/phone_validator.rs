//! The `phone` validator: what a declared region accepts, what it refuses, and
//! what it stores.
//!
//! The evaluation lives here rather than in the planner because it is a pure
//! value contract — a string in, an E.164 string or a rejection out — and
//! because the region it reads is metadata, which this crate owns.

use donat_metadata::{PermissionValidator, PhoneRegion, normalize_phone};

fn region(code: &str) -> PhoneRegion {
    PhoneRegion::parse(code).expect("a declared region parses")
}

/// The spec's proof: invalid numbers reject, valid ones store E.164, and the
/// same number written five ways collapses to one stored value.
#[test]
fn phone_validator_rejects_and_normalizes() {
    let de = region("DE");

    // One number, five spellings — national, spaced, punctuated,
    // international with a plus, and international with the German trunk
    // prefix. A uniqueness constraint over the column only means something if
    // all five land on the same string.
    let spellings = [
        "030 1234567",
        "030-123 4567",
        "(030) 1234567",
        "+49 30 1234567",
        "+49 (0)30 123 4567",
    ];
    let stored: Vec<String> = spellings
        .iter()
        .map(|value| {
            normalize_phone(value, &de)
                .unwrap_or_else(|error| panic!("{value} is a valid German number: {error}"))
        })
        .collect();
    assert_eq!(
        stored,
        vec!["+49301234567".to_string(); 5],
        "every spelling of one number must collapse to one stored value"
    );

    // Rejections. `+49 30 1` is too short for any German numbering plan;
    // `+49 1111 111111` is well-formed, long enough, and still not a valid
    // number of any type — the case a length check or a regex misses.
    for value in ["", "not a phone number", "+49 30 1", "+49 1111 111111"] {
        assert!(
            normalize_phone(value, &de).is_err(),
            "{value:?} must be refused"
        );
    }

    // Normalization is idempotent: storing an already-stored value is a
    // no-op, so a row rewritten by an update does not drift.
    assert_eq!(
        normalize_phone("+49301234567", &de).expect("E.164 input is accepted"),
        "+49301234567"
    );
}

/// The region is deploy-time metadata. Nothing that a caller can spell — a
/// header name, a session variable, a role, a lowercase locale — resolves to a
/// region, so a declaration that tries to defer the choice to the request
/// refuses publication instead of silently reading one.
#[test]
fn phone_region_is_deploy_time() {
    for spelling in [
        "X-Donat-Region",
        "x-donat-region",
        "X-Donat-Role",
        "de",
        "DEU",
        "",
        "$region",
    ] {
        assert!(
            PhoneRegion::parse(spelling).is_err(),
            "{spelling:?} is not a declared region"
        );
    }

    // The declared region is what decides validity: a national German number
    // is valid under DE and not a US number, and the value carries no way to
    // ask for a different one.
    let national = "030 1234567";
    assert_eq!(
        normalize_phone(national, &region("DE")).expect("valid in the declared region"),
        "+49301234567"
    );
    assert!(
        normalize_phone(national, &region("US")).is_err(),
        "a national number is read in the declared region, never in the caller's"
    );

    // An international number is region-independent, so a declared region can
    // never override the country a caller wrote out in full.
    assert_eq!(
        normalize_phone("+1 202 555 0192", &region("DE")).expect("+1 is a US number"),
        "+12025550192"
    );
}

/// A `phone` entry is one predicate spelling among the others, and it carries
/// its own message like every validator.
#[test]
fn a_phone_entry_parses_as_one_validator_spelling() {
    let validators: Vec<PermissionValidator> = serde_yaml::from_str(
        r#"
- phone: { column: contact_phone, region: DE }
  message: contact_phone must be a valid phone number
"#,
    )
    .expect("a phone validator parses");
    let phone = validators[0]
        .phone
        .as_ref()
        .expect("the entry declares a phone validator");
    assert_eq!(phone.column, "contact_phone");
    assert_eq!(phone.region, "DE");
    assert_eq!(
        validators[0].message,
        "contact_phone must be a valid phone number"
    );
}

/// The embedded numbering-plan database decides which numbers are valid, so a
/// bump to it can turn a number that was accepted yesterday into one that is
/// refused today. It is therefore pinned exactly: the crate version's build
/// metadata (`+9.0.33`) *is* the libphonenumber metadata version, and this
/// test fails the moment the resolved dependency moves.
///
/// What this does **not** yet do is put that version in a deployment
/// fingerprint, because the engine has no engine-wide fingerprint to put it in
/// — the fingerprints that exist are per connector operation and per process
/// revision. Until one exists, an exact pin plus a failing test is the whole
/// of the guarantee: the version cannot change without a deliberate commit.
#[test]
fn phone_metadata_version_is_pinned() {
    const PINNED: &str = "0.3.10+9.0.33";

    let lock = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"),
    )
    .expect("the workspace lock file is readable");
    let resolved = lock
        .split("[[package]]")
        .find(|package| package.contains("\nname = \"phonenumber\"\n"))
        .and_then(|package| {
            package
                .lines()
                .find_map(|line| line.strip_prefix("version = \""))
        })
        .map(|version| version.trim_end_matches('"'))
        .expect("phonenumber is a resolved dependency");

    assert_eq!(
        resolved, PINNED,
        "the embedded numbering-plan database version changed; that changes which \
         numbers are valid, so it is a deliberate commit — update PINNED here and \
         say so in the change"
    );
}
