//! Exact engine/analyzer compatibility evidence for immutable postings.
//!
//! Source checksums alone cannot establish tokenization compatibility. The
//! manifest additionally records compiled engine/analyzer versions and the
//! reviewed native adapter/profile revision. The dependency guard binds all
//! embedded Lindera dictionary packages to the same pinned release family.

use serde::{Deserialize, Serialize};

use crate::SearchError;

const ANALYZER_REVISION: u16 = 1;
const TANTIVY_PATCH_REVISION: &str = "nibleaf-lru-0.18.5-v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationRuntime {
    /// Tantivy's compiled version string includes its on-disk index format.
    pub tantivy: String,
    pub tantivy_dependency_patch: String,
    pub lindera: String,
    /// Normal mode, no user dictionary or filters, embedded IPADIC/CC-CEDICT,
    /// whitespace omitted, unknown-word ladder disabled; native adapter v1.
    pub analyzer_revision: u16,
}

impl GenerationRuntime {
    pub(crate) fn ensure_current(&self) -> Result<(), SearchError> {
        if self != &current_runtime() {
            return Err(SearchError::Integrity(
                "generation engine/analyzer runtime is incompatible",
            ));
        }
        Ok(())
    }
}

pub(crate) fn current_runtime() -> GenerationRuntime {
    GenerationRuntime {
        tantivy: tantivy::version_string().to_owned(),
        tantivy_dependency_patch: TANTIVY_PATCH_REVISION.to_owned(),
        lindera: lindera::get_version().to_owned(),
        analyzer_revision: ANALYZER_REVISION,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_identity_is_exact_and_closed_not_a_semver_range() -> Result<(), SearchError> {
        let runtime = current_runtime();
        assert_eq!(runtime.tantivy, "tantivy v0.26.2, index_format v7");
        assert_eq!(runtime.lindera, "6.2.0");
        runtime.ensure_current()?;
        for changed in [
            GenerationRuntime {
                tantivy: "tantivy v0.25.0, index_format v7".to_owned(),
                ..runtime.clone()
            },
            GenerationRuntime {
                lindera: "2.3.4".to_owned(),
                ..runtime.clone()
            },
            GenerationRuntime {
                analyzer_revision: 2,
                ..runtime.clone()
            },
            GenerationRuntime {
                tantivy_dependency_patch: "unreviewed".to_owned(),
                ..runtime.clone()
            },
        ] {
            assert!(matches!(
                changed.ensure_current(),
                Err(SearchError::Integrity(_))
            ));
        }
        let mut serialized = serde_json::to_value(runtime)?;
        serialized["unreviewed"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<GenerationRuntime>(serialized).is_err());
        Ok(())
    }
}
