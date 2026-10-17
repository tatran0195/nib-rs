//! API-process query limits and cooperative checkpoints, not artifact formats.

use std::{env, time::Duration};

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::SearchError;

const MIB: usize = 1024 * 1024;

/// Trusted process configuration. Not a browser input or publication ceiling.
/// Construction of a query runtime validates every field, including manually
/// constructed values. These bound admitted work, not the process's exact RSS.
#[derive(Clone, Copy, Debug)]
pub struct SearchQueryConfig {
    pub max_in_flight: usize,
    pub timeout: Duration,
    pub max_snapshot_bytes: usize,
    pub max_manifest_bytes: usize,
    pub max_documents: usize,
    pub max_source_bytes: usize,
}

impl Default for SearchQueryConfig {
    fn default() -> Self {
        Self {
            max_in_flight: 2,
            timeout: Duration::from_secs(5),
            max_snapshot_bytes: 16 * MIB,
            max_manifest_bytes: 16 * MIB,
            max_documents: 50_000,
            max_source_bytes: 64 * MIB,
        }
    }
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
#[error("invalid search query configuration: {0}")]
pub struct SearchQueryConfigError(pub(crate) &'static str);

impl SearchQueryConfig {
    /// Reads API-only operational limits. Absent values use defaults; empty,
    /// non-Unicode and malformed values fail startup rather than being ignored.
    ///
    /// # Errors
    /// Returns the setting name (not its value) for invalid configuration.
    pub fn from_env() -> Result<Self, SearchQueryConfigError> {
        Self::from_values(|name| match env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(env::VarError::NotPresent) => Ok(None),
            Err(env::VarError::NotUnicode(_)) => Err(SearchQueryConfigError(name)),
        })
    }

    fn from_values(
        read: impl Fn(&'static str) -> Result<Option<String>, SearchQueryConfigError>,
    ) -> Result<Self, SearchQueryConfigError> {
        let defaults = Self::default();
        let read_number = |name, default| {
            read(name)?.map_or(Ok(default), |value| {
                value.parse().map_err(|_| SearchQueryConfigError(name))
            })
        };
        let config = Self {
            max_in_flight: read_number("NIBLEAF_SEARCH_QUERY_CONCURRENCY", defaults.max_in_flight)?,
            timeout: Duration::from_millis(
                u64::try_from(read_number("NIBLEAF_SEARCH_QUERY_TIMEOUT_MS", 5_000)?)
                    .map_err(|_| SearchQueryConfigError("NIBLEAF_SEARCH_QUERY_TIMEOUT_MS"))?,
            ),
            max_snapshot_bytes: read_number(
                "NIBLEAF_SEARCH_QUERY_MAX_SNAPSHOT_BYTES",
                defaults.max_snapshot_bytes,
            )?,
            max_manifest_bytes: read_number(
                "NIBLEAF_SEARCH_QUERY_MAX_MANIFEST_BYTES",
                defaults.max_manifest_bytes,
            )?,
            max_documents: read_number(
                "NIBLEAF_SEARCH_QUERY_MAX_DOCUMENTS",
                defaults.max_documents,
            )?,
            max_source_bytes: read_number(
                "NIBLEAF_SEARCH_QUERY_MAX_SOURCE_BYTES",
                defaults.max_source_bytes,
            )?,
        };
        config.validate()?;
        Ok(config)
    }

    pub(crate) fn validate(self) -> Result<(), SearchQueryConfigError> {
        for (name, value, maximum) in [
            ("NIBLEAF_SEARCH_QUERY_CONCURRENCY", self.max_in_flight, 32),
            (
                "NIBLEAF_SEARCH_QUERY_MAX_SNAPSHOT_BYTES",
                self.max_snapshot_bytes,
                128 * MIB,
            ),
            (
                "NIBLEAF_SEARCH_QUERY_MAX_MANIFEST_BYTES",
                self.max_manifest_bytes,
                128 * MIB,
            ),
            (
                "NIBLEAF_SEARCH_QUERY_MAX_DOCUMENTS",
                self.max_documents,
                500_000,
            ),
            (
                "NIBLEAF_SEARCH_QUERY_MAX_SOURCE_BYTES",
                self.max_source_bytes,
                1024 * MIB,
            ),
        ] {
            if value == 0 || value > maximum {
                return Err(SearchQueryConfigError(name));
            }
        }
        if self.timeout.is_zero() || self.timeout > Duration::from_secs(30) {
            return Err(SearchQueryConfigError("NIBLEAF_SEARCH_QUERY_TIMEOUT_MS"));
        }
        Ok(())
    }
}

/// Only the runtime creates this guard. A cancelled response does not free the
/// runtime's capacity permit while blocking work is still running.
#[derive(Clone, Debug)]
pub(crate) struct QueryBudget {
    pub(crate) config: SearchQueryConfig,
    pub(crate) started: Instant,
    pub(crate) deadline: Instant,
    pub(crate) cancellation: CancellationToken,
}

impl QueryBudget {
    pub(crate) fn check(&self) -> Result<(), SearchError> {
        if Instant::now() >= self.deadline {
            return Err(SearchError::Deadline);
        }
        if self.cancellation.is_cancelled() {
            return Err(SearchError::Cancelled);
        }
        Ok(())
    }

    pub(crate) fn statement_timeout(&self) -> Result<String, SearchError> {
        self.check()?;
        Ok(postgres_timeout(
            self.deadline.saturating_duration_since(Instant::now()),
        ))
    }
}

// Round up: flooring could let the server cancel just before the client
// deadline, turning an operational budget into a premature database error.
fn postgres_timeout(remaining: Duration) -> String {
    format!("{}ms", remaining.as_nanos().div_ceil(1_000_000).max(1))
}

pub(crate) fn checkpoint(budget: Option<&QueryBudget>) -> Result<(), SearchError> {
    budget.map_or(Ok(()), QueryBudget::check)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_every_manual_limit_are_validated() -> Result<(), SearchQueryConfigError> {
        SearchQueryConfig::default().validate()?;
        for invalid in [
            SearchQueryConfig {
                max_in_flight: 0,
                ..SearchQueryConfig::default()
            },
            SearchQueryConfig {
                timeout: Duration::ZERO,
                ..SearchQueryConfig::default()
            },
            SearchQueryConfig {
                max_snapshot_bytes: 0,
                ..SearchQueryConfig::default()
            },
            SearchQueryConfig {
                max_manifest_bytes: 129 * MIB,
                ..SearchQueryConfig::default()
            },
            SearchQueryConfig {
                max_documents: 500_001,
                ..SearchQueryConfig::default()
            },
            SearchQueryConfig {
                max_source_bytes: 1024 * MIB + 1,
                ..SearchQueryConfig::default()
            },
        ] {
            assert!(invalid.validate().is_err());
        }
        Ok(())
    }

    #[test]
    fn malformed_env_is_not_silently_defaulted() -> Result<(), SearchQueryConfigError> {
        let defaults = SearchQueryConfig::from_values(|_| Ok(None))?;
        assert_eq!(defaults.max_in_flight, 2);
        for value in ["", " 2 ", "-1", "0", "33", "two"] {
            let error = SearchQueryConfig::from_values(|name| {
                Ok((name == "NIBLEAF_SEARCH_QUERY_CONCURRENCY").then(|| value.to_owned()))
            });
            assert_eq!(
                error.err(),
                Some(SearchQueryConfigError("NIBLEAF_SEARCH_QUERY_CONCURRENCY"))
            );
        }
        Ok(())
    }

    #[test]
    fn postgres_timeout_rounds_up_instead_of_cancelling_before_the_deadline() {
        for (remaining, expected) in [
            (Duration::ZERO, "1ms"),
            (Duration::from_nanos(1), "1ms"),
            (Duration::from_millis(1), "1ms"),
            (Duration::from_nanos(1_000_001), "2ms"),
            (Duration::from_secs(30), "30000ms"),
        ] {
            assert_eq!(postgres_timeout(remaining), expected);
        }
    }

    #[tokio::test]
    async fn cancellation_and_expired_deadlines_are_distinct() {
        let mut budget = QueryBudget {
            config: SearchQueryConfig::default(),
            started: Instant::now(),
            deadline: Instant::now() + Duration::from_secs(1),
            cancellation: CancellationToken::new(),
        };
        assert!(budget.check().is_ok());
        budget.deadline = Instant::now();
        assert!(matches!(budget.check(), Err(SearchError::Deadline)));
        budget.deadline = Instant::now() + Duration::from_secs(1);
        budget.cancellation.cancel();
        assert!(matches!(budget.check(), Err(SearchError::Cancelled)));
    }
}
