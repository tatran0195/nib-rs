use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AppError;

/// Existing Nibleaf tables use opaque text identifiers (historical CUIDs). The
/// Rust platform retains that storage contract while refusing empty/control-char
/// IDs at its own boundary. New IDs are `UUIDv7` strings, which are sortable and
/// safe to expose as opaque API identifiers.
fn validate_id(value: &str, kind: &'static str) -> Result<(), AppError> {
    if !(1..=191).contains(&value.len())
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(AppError::validation(format!("invalid {kind}")));
    }
    Ok(())
}

macro_rules! opaque_id {
    ($name:ident, $label:literal) => {
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7().to_string())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl TryFrom<String> for $name {
            type Error = AppError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                validate_id(&value, $label)?;
                Ok(Self(value))
            }
        }

        impl FromStr for $name {
            type Err = AppError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::try_from(value.to_owned())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

opaque_id!(UserId, "user id");
opaque_id!(OrganizationId, "organization id");
opaque_id!(ProjectId, "project id");
opaque_id!(PageId, "page id");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_boundary_breaking_identifier() {
        assert!(ProjectId::try_from("contains space".to_owned()).is_err());
        assert!(ProjectId::try_from("\u{0000}".to_owned()).is_err());
    }

    #[test]
    fn new_identifier_is_opaque_and_nonempty() {
        let identifier = ProjectId::new();
        assert_ne!(identifier.as_str(), "");
    }
}
