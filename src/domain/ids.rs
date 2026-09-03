//! Typed, validated identifiers used by the v0.2 domain model.

use std::fmt;
use std::str::FromStr;

/// The identifier categories defined by the profile schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IdentifierKind {
    Profile,
    Resource,
    Store,
}

impl fmt::Display for IdentifierKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Profile => formatter.write_str("profile"),
            Self::Resource => formatter.write_str("resource"),
            Self::Store => formatter.write_str("store"),
        }
    }
}

/// The reason an identifier fails the v0.2 identifier grammar.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IdentifierViolation {
    Empty,
    TooLong,
    InvalidFirstCharacter,
    InvalidCharacter,
}

impl fmt::Display for IdentifierViolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("is empty"),
            Self::TooLong => formatter.write_str("is longer than 63 characters"),
            Self::InvalidFirstCharacter => {
                formatter.write_str("does not begin with a lowercase ASCII letter")
            }
            Self::InvalidCharacter => formatter.write_str(
                "contains a character other than a lowercase ASCII letter, digit, or hyphen",
            ),
        }
    }
}

/// An invalid profile, resource, or store identifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IdentifierError {
    kind: IdentifierKind,
    value: String,
    violation: IdentifierViolation,
}

impl IdentifierError {
    /// The category of identifier that failed validation.
    pub(crate) fn kind(&self) -> IdentifierKind {
        self.kind
    }

    /// The rejected identifier text.
    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    /// The violated grammar rule.
    pub(crate) fn violation(&self) -> IdentifierViolation {
        self.violation
    }
}

impl fmt::Display for IdentifierError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid {} identifier {:?}: {}",
            self.kind, self.value, self.violation
        )
    }
}

impl std::error::Error for IdentifierError {}

macro_rules! identifier_type {
    ($name:ident, $kind:expr) => {
        #[doc = concat!("A validated ", stringify!($name), ".")]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub(crate) struct $name(String);

        impl $name {
            /// Validates the identifier grammar from the Profile specification.
            pub(crate) fn parse(value: impl Into<String>) -> Result<Self, IdentifierError> {
                validate_identifier($kind, value.into()).map(Self)
            }

            /// Returns the validated identifier text.
            pub(crate) fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdentifierError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }
    };
}

identifier_type!(ProfileId, IdentifierKind::Profile);
identifier_type!(ResourceId, IdentifierKind::Resource);
identifier_type!(StoreId, IdentifierKind::Store);

/// A fully qualified resource ID has exactly one `<profile-id>/<resource-id>` boundary.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct FullyQualifiedResourceId(String);

impl FullyQualifiedResourceId {
    /// Combines independently validated profile and resource IDs.
    pub(crate) fn new(profile: &ProfileId, resource: &ResourceId) -> Self {
        Self(format!("{profile}/{resource}"))
    }

    /// Parses and validates a persisted fully qualified resource ID.
    pub(crate) fn parse(value: &str) -> Result<Self, FullyQualifiedResourceIdError> {
        let (profile, resource) = value
            .split_once('/')
            .ok_or(FullyQualifiedResourceIdError::MissingSeparator)?;

        if resource.contains('/') {
            return Err(FullyQualifiedResourceIdError::MultipleSeparators);
        }

        let profile =
            ProfileId::parse(profile).map_err(FullyQualifiedResourceIdError::InvalidProfileId)?;
        let resource = ResourceId::parse(resource)
            .map_err(FullyQualifiedResourceIdError::InvalidResourceId)?;

        Ok(Self::new(&profile, &resource))
    }

    /// Returns the stable resource identity used across lifecycle boundaries.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FullyQualifiedResourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for FullyQualifiedResourceId {
    type Err = FullyQualifiedResourceIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// The reason a fully qualified resource ID is invalid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FullyQualifiedResourceIdError {
    MissingSeparator,
    MultipleSeparators,
    InvalidProfileId(IdentifierError),
    InvalidResourceId(IdentifierError),
}

impl fmt::Display for FullyQualifiedResourceIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSeparator => formatter.write_str(
                "fully qualified resource ID must contain one '/' between profile and resource IDs",
            ),
            Self::MultipleSeparators => formatter.write_str(
                "fully qualified resource ID must contain exactly one '/' between profile and resource IDs",
            ),
            Self::InvalidProfileId(error) => write!(formatter, "invalid profile portion: {error}"),
            Self::InvalidResourceId(error) => write!(formatter, "invalid resource portion: {error}"),
        }
    }
}

impl std::error::Error for FullyQualifiedResourceIdError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidProfileId(error) | Self::InvalidResourceId(error) => Some(error),
            Self::MissingSeparator | Self::MultipleSeparators => None,
        }
    }
}

fn validate_identifier(kind: IdentifierKind, value: String) -> Result<String, IdentifierError> {
    let violation = match value.as_bytes() {
        [] => Some(IdentifierViolation::Empty),
        bytes if bytes.len() > 63 => Some(IdentifierViolation::TooLong),
        [first, ..] if !first.is_ascii_lowercase() => {
            Some(IdentifierViolation::InvalidFirstCharacter)
        }
        bytes
            if bytes.iter().any(|byte| {
                !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && *byte != b'-'
            }) =>
        {
            Some(IdentifierViolation::InvalidCharacter)
        }
        _ => None,
    };

    match violation {
        Some(violation) => Err(IdentifierError {
            kind,
            value,
            violation,
        }),
        None => Ok(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_types_accept_the_profile_grammar() {
        let maximum_length = format!("a{}", "b".repeat(62));

        assert_eq!(
            ProfileId::parse("workstation-2").unwrap().as_str(),
            "workstation-2"
        );
        assert_eq!(
            ResourceId::parse("git-config").unwrap().as_str(),
            "git-config"
        );
        assert_eq!(
            StoreId::parse(maximum_length.clone()).unwrap().as_str(),
            maximum_length
        );
    }

    #[test]
    fn identifier_types_reject_each_invalid_grammar_class() {
        let too_long = format!("a{}", "b".repeat(63));
        let cases = [
            ("", IdentifierViolation::Empty),
            (too_long.as_str(), IdentifierViolation::TooLong),
            (
                "1starts-with-a-digit",
                IdentifierViolation::InvalidFirstCharacter,
            ),
            (
                "-starts-with-a-hyphen",
                IdentifierViolation::InvalidFirstCharacter,
            ),
            ("Uppercase", IdentifierViolation::InvalidFirstCharacter),
            ("contains_underscore", IdentifierViolation::InvalidCharacter),
            ("non-ascii-é", IdentifierViolation::InvalidCharacter),
        ];

        for (value, expected_violation) in cases {
            let profile_error = ProfileId::parse(value).unwrap_err();
            let resource_error = ResourceId::parse(value).unwrap_err();
            let store_error = StoreId::parse(value).unwrap_err();

            assert_eq!(profile_error.kind(), IdentifierKind::Profile);
            assert_eq!(resource_error.kind(), IdentifierKind::Resource);
            assert_eq!(store_error.kind(), IdentifierKind::Store);
            assert_eq!(profile_error.value(), value);
            assert_eq!(resource_error.value(), value);
            assert_eq!(store_error.value(), value);
            assert_eq!(profile_error.violation(), expected_violation);
            assert_eq!(resource_error.violation(), expected_violation);
            assert_eq!(store_error.violation(), expected_violation);
        }
    }

    #[test]
    fn fully_qualified_resource_id_is_composed_from_validated_parts() {
        let profile = ProfileId::parse("base").unwrap();
        let resource = ResourceId::parse("git-config").unwrap();

        let resource_id = FullyQualifiedResourceId::new(&profile, &resource);

        assert_eq!(resource_id.as_str(), "base/git-config");
        assert_eq!(resource_id.to_string(), "base/git-config");
        assert_eq!(
            FullyQualifiedResourceId::parse("base/git-config").unwrap(),
            resource_id
        );
    }

    #[test]
    fn fully_qualified_resource_id_rejects_invalid_structure_and_parts() {
        assert_eq!(
            FullyQualifiedResourceId::parse("base").unwrap_err(),
            FullyQualifiedResourceIdError::MissingSeparator
        );
        assert_eq!(
            FullyQualifiedResourceId::parse("base/git/config").unwrap_err(),
            FullyQualifiedResourceIdError::MultipleSeparators
        );
        assert!(matches!(
            FullyQualifiedResourceId::parse("Base/git-config"),
            Err(FullyQualifiedResourceIdError::InvalidProfileId(_))
        ));
        assert!(matches!(
            FullyQualifiedResourceId::parse("base/Git-config"),
            Err(FullyQualifiedResourceIdError::InvalidResourceId(_))
        ));
    }

    #[test]
    fn fully_qualified_resource_ids_sort_lexicographically() {
        let mut resource_ids = [
            FullyQualifiedResourceId::parse("zeta/alpha").unwrap(),
            FullyQualifiedResourceId::parse("base/zulu").unwrap(),
            FullyQualifiedResourceId::parse("base/alpha").unwrap(),
        ];

        resource_ids.sort();

        assert_eq!(
            resource_ids
                .iter()
                .map(FullyQualifiedResourceId::as_str)
                .collect::<Vec<_>>(),
            ["base/alpha", "base/zulu", "zeta/alpha"]
        );
    }
}
