//! Evaluation of the `phone` validator.
//!
//! A phone number is the one value contract that cannot be written as a
//! predicate: whether `+49 1111 111111` is a real number is a property of a
//! versioned database of national numbering plans, not of its length or its
//! characters. `phonenumber` carries that database, so this module is a thin,
//! pure shell around it — a string and a declared region in, an E.164 string
//! or a rejection out.
//!
//! Two properties are load-bearing and are why this lives in the metadata
//! crate rather than in the planner.
//!
//! *The region is deploy-time only.* A [`PhoneRegion`] can be built one way,
//! from a declared CLDR region code, and nothing a request carries — a header,
//! a session variable, a role, a value in the submitted object — parses as
//! one. A declaration that tried to defer the region to the request would fail
//! to parse and refuse publication.
//!
//! *Normalization happens before the statement is built.* The planner rewrites
//! the submitted value to its E.164 form and the statement carries that, so
//! one number in several spellings is one stored value and a uniqueness
//! constraint over the column means what it says. It costs no extra
//! statement, no round trip, and nothing in SQL.

use std::fmt;

use phonenumber::country;

/// A declared default region for numbers written without an international
/// prefix.
///
/// The only constructor is [`PhoneRegion::parse`], over a CLDR region code
/// exactly as the metadata spells it. That is the whole guarantee that the
/// region is deploy-time data: a value of this type can only have come from
/// metadata that was accepted at publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhoneRegion(country::Id);

impl PhoneRegion {
    /// Resolve a declared region code, e.g. `DE`.
    ///
    /// The spelling is the uppercase two-letter CLDR code and nothing else, so
    /// `de`, `DEU` and anything shaped like a header name are errors rather
    /// than lenient matches.
    pub fn parse(code: &str) -> Result<Self, PhoneRegionError> {
        code.parse::<country::Id>()
            .map(Self)
            .map_err(|_| PhoneRegionError {
                code: code.to_owned(),
            })
    }

    /// The declared code, as it is spelled in metadata.
    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }
}

impl fmt::Display for PhoneRegion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An undeclarable region. This is a deployment error: it is raised while
/// metadata is compiled, never while a request is served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneRegionError {
    code: String,
}

impl fmt::Display for PhoneRegionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "'{}' is not a region code; a `phone` validator declares an uppercase two-letter region, e.g. DE",
            self.code
        )
    }
}

impl std::error::Error for PhoneRegionError {}

/// Why one submitted value is not an acceptable phone number.
///
/// The caller never reads this: a rejected value is reported with the
/// validator's own `message`, like every other validator. The variants exist
/// so a diagnostic can say which of the two very different failures happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhoneRejection {
    /// The value is not a phone number at all in the declared region.
    Unparseable,
    /// The value parses, but is not a valid number of any type — a numbering
    /// plan question, not a syntax one.
    NotValid,
}

impl fmt::Display for PhoneRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unparseable => f.write_str("the value is not a phone number"),
            Self::NotValid => f.write_str("the value is not a valid phone number"),
        }
    }
}

impl std::error::Error for PhoneRejection {}

/// Parse one submitted value in the declared region and return its E.164 form.
///
/// Validity is checked, not just syntax: a number that parses but that no
/// numbering plan assigns is refused, because storing it would mean the column
/// holds something that can never be dialled.
pub fn normalize_phone(value: &str, region: &PhoneRegion) -> Result<String, PhoneRejection> {
    let number =
        phonenumber::parse(Some(region.0), value).map_err(|_| PhoneRejection::Unparseable)?;
    if !phonenumber::is_valid(&number) {
        return Err(PhoneRejection::NotValid);
    }
    Ok(number.format().mode(phonenumber::Mode::E164).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Normalization is a total function on strings: no input panics, however
    /// hostile. A validator that can panic is a 500 waiting for a fuzzer.
    #[test]
    fn no_input_panics() {
        let region = PhoneRegion::parse("DE").expect("DE is a region");
        for value in [
            "",
            " ",
            "+",
            "++49",
            "\u{0}\u{1}",
            "☎",
            "+49301234567;ext=99",
            &"9".repeat(4096),
        ] {
            let _ = normalize_phone(value, &region);
        }
    }

    #[test]
    fn a_region_round_trips_through_its_declared_spelling() {
        let region = PhoneRegion::parse("GB").expect("GB is a region");
        assert_eq!(region.as_str(), "GB");
        assert_eq!(region.to_string(), "GB");
    }
}
