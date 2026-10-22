//! Phone numbers. Contacts store them in E.164 form, e.g. `+905321234567`.

use std::fmt;
use std::str::FromStr;

use phonenumber::country;

use crate::error::{Error, Result};

/// The country whose conventions apply to numbers written without a country
/// code: with `TR`, `0532 123 45 67` is read as `+905321234567`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region(country::Id);

impl Region {
    pub const TURKEY: Region = Region(country::Id::TR);
}

impl FromStr for Region {
    type Err = String;

    /// An ISO 3166-1 alpha-2 country code, e.g. `TR` or `gb`.
    fn from_str(code: &str) -> std::result::Result<Self, Self::Err> {
        code.trim()
            .to_ascii_uppercase()
            .parse::<country::Id>()
            .map(Region)
            .map_err(|_| format!("unknown region {code:?}; expected a country code such as TR"))
    }
}

impl fmt::Display for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_ref())
    }
}

/// `input` in E.164 form. A number with a country code (`+44 20 7946 0958`,
/// `0044 20 7946 0958`) keeps it; one without is read as a number in `region`.
/// Numbers that libphonenumber's metadata doesn't consider valid are rejected.
pub fn normalize(input: &str, region: Region) -> Result<String> {
    let invalid = || {
        Error::Invalid(format!(
            "invalid phone number {input:?}: use international format like +905321234567, \
             or a number valid in {region}"
        ))
    };
    let number = phonenumber::parse(Some(region.0), input).map_err(|_| invalid())?;
    if !number.is_valid() {
        return Err(invalid());
    }
    Ok(number.format().mode(phonenumber::Mode::E164).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_to_e164() {
        let turkey = Region::TURKEY;
        for input in [
            "0532 123 45 67",
            "532 123 45 67",
            "(0532) 123-45-67",
            "+90 532 123 45 67",
            "0090 532 123 45 67",
            "+905321234567",
        ] {
            assert_eq!(
                normalize(input, turkey).ok().as_deref(),
                Some("+905321234567"),
                "{input}"
            );
        }
        assert_eq!(
            normalize("+44 20 7946 0958", turkey).ok().as_deref(),
            Some("+442079460958")
        );
        let britain: Region = "gb".parse().expect("valid region");
        assert_eq!(
            normalize("020 7946 0958", britain).ok().as_deref(),
            Some("+442079460958")
        );
    }

    #[test]
    fn rejects_invalid_numbers() {
        for input in ["", "call me", "123", "0532 123", "+90 532 123 45 67 89 01"] {
            assert!(normalize(input, Region::TURKEY).is_err(), "{input}");
        }
    }

    #[test]
    fn parses_regions() {
        assert_eq!("tr".parse::<Region>(), Ok(Region::TURKEY));
        assert_eq!(Region::TURKEY.to_string(), "TR");
        assert!("XX".parse::<Region>().is_err());
    }
}
