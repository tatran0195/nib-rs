//! Immutable publication ceilings intersected with current live restrictions.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Number, Value};

use crate::{MAX_RESULTS, published::PublishedSearchError, snapshot::SnapshotScope};

const DEFAULT_RESULT_CAP: usize = 12;

/// Server-resolved search scope; not an HTTP request/response or authorization
/// ticket. Client selectors cannot construct or widen this context.
#[derive(Debug, Eq, PartialEq)]
pub struct PublishedSearchContext {
    version_id: String,
    version_slug: String,
    languages: BTreeSet<String>,
    result_limit: usize,
}

impl PublishedSearchContext {
    #[must_use]
    pub fn version_id(&self) -> &str {
        &self.version_id
    }

    #[must_use]
    pub fn version_slug(&self) -> &str {
        &self.version_slug
    }

    /// Exact intersection of published eligibility and current enabled codes.
    /// An empty set is an intentional empty corpus, never "all languages".
    #[must_use]
    pub const fn languages(&self) -> &BTreeSet<String> {
        &self.languages
    }

    /// Minimum of the explicit/default request, published ceiling and live cap.
    #[must_use]
    pub const fn result_limit(&self) -> usize {
        self.result_limit
    }
}

#[derive(Deserialize)]
pub(crate) struct LiveSearchControls {
    config: Option<Value>,
    languages: Vec<LiveLanguage>,
}

#[derive(Deserialize)]
struct LiveLanguage {
    code: String,
    enabled: bool,
}

#[derive(Default, Deserialize)]
struct SearchConfig {
    search: Option<SearchCap>,
}

#[derive(Deserialize)]
struct SearchCap {
    #[serde(rename = "maxResults", default = "default_cap")]
    max_results: Number,
}

fn default_cap() -> Number {
    Number::from(DEFAULT_RESULT_CAP)
}

fn result_cap(config: Option<&Value>) -> Result<usize, PublishedSearchError> {
    let config = config
        .filter(|config| !config.is_null())
        .map(SearchConfig::deserialize)
        .transpose()
        .map_err(PublishedSearchError::Controls)?
        .unwrap_or_default();
    let number = config
        .search
        .map_or_else(default_cap, |cap| cap.max_results);
    // JSONB can preserve decimal scale: 7, 7.0 and 7e0 are all the valid
    // integer setting seven. A tiny bounded lookup avoids lossy float casts.
    let number = number.as_f64().ok_or(PublishedSearchError::Unavailable(
        "result cap is not a bounded integer",
    ))?;
    (1..=MAX_RESULTS)
        .find(|candidate| {
            u32::try_from(*candidate)
                .is_ok_and(|candidate| number.total_cmp(&f64::from(candidate)).is_eq())
        })
        .ok_or(PublishedSearchError::Unavailable(
            "result cap must be an integer between 1 and 50",
        ))
}

fn live_languages(
    controls: &LiveSearchControls,
) -> Result<BTreeMap<&str, bool>, PublishedSearchError> {
    let mut languages = BTreeMap::new();
    for language in &controls.languages {
        if language.code.is_empty()
            || language.code.len() > 191
            || language.code.trim() != language.code
            || languages
                .insert(language.code.as_str(), language.enabled)
                .is_some()
        {
            return Err(PublishedSearchError::Unavailable(
                "live language controls are malformed or ambiguous",
            ));
        }
    }
    Ok(languages)
}

pub(crate) fn resolve_context(
    scope: &SnapshotScope,
    live: &LiveSearchControls,
    requested_version: Option<&str>,
    requested_language: Option<&str>,
    requested_limit: Option<usize>,
) -> Result<PublishedSearchContext, PublishedSearchError> {
    if requested_limit.is_some_and(|limit| !(1..=MAX_RESULTS).contains(&limit)) {
        return Err(PublishedSearchError::InvalidRequest(
            "limit must be between 1 and 50",
        ));
    }
    let default_version = scope
        .versions
        .iter()
        .find(|version| version.is_default)
        .ok_or(PublishedSearchError::Unavailable(
            "published default version is missing",
        ))?;
    let version = requested_version
        .and_then(|slug| scope.versions.iter().find(|version| version.slug == slug))
        .unwrap_or(default_version);
    let current = live_languages(live)?;
    let eligible: BTreeSet<String> = scope
        .languages
        .iter()
        .filter(|language| {
            language.enabled && current.get(language.code.as_str()).copied() == Some(true)
        })
        .map(|language| language.code.clone())
        .collect();
    let languages = if let Some(requested) = requested_language {
        let default = scope
            .languages
            .iter()
            .find(|language| language.is_default)
            .ok_or(PublishedSearchError::Unavailable(
                "published default language is missing",
            ))?;
        let language = if eligible.contains(requested) {
            requested
        } else {
            default.code.as_str()
        };
        // Never force a live-disabled/missing default back into the corpus or
        // silently substitute a different locale when fallback is unavailable.
        eligible
            .into_iter()
            .filter(|candidate| candidate == language)
            .collect()
    } else {
        eligible
    };
    let ceiling = result_cap(scope.project.config.as_ref())?.min(result_cap(live.config.as_ref())?);
    Ok(PublishedSearchContext {
        version_id: version.id.clone(),
        version_slug: version.slug.clone(),
        languages,
        result_limit: requested_limit.unwrap_or(ceiling).min(ceiling),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::snapshot::snapshot_scope;

    fn snapshot() -> Value {
        json!({
            "schema_version":2, "project":{"id":"project-a", "config":{"search":{"maxResults":7}}},
            "versions":[
                {"id":"branch-main", "slug":"main", "is_default":true},
                {"id":"branch-next", "slug":"next", "is_default":false}
            ],
            "languages":[
                {"id":"en", "code":"en", "is_default":true, "enabled":true},
                {"id":"gb", "code":"en-GB", "is_default":false, "enabled":true},
                {"id":"fr", "code":"fr", "is_default":false, "enabled":true},
                {"id":"ja", "code":"ja", "is_default":false, "enabled":false}
            ]
        })
    }

    fn live() -> Result<LiveSearchControls, serde_json::Error> {
        serde_json::from_value(json!({
            "config":{"search":{"maxResults":50}},
            "languages":[
                {"code":"en", "enabled":true}, {"code":"en-GB", "enabled":true},
                {"code":"fr", "enabled":false}, {"code":"ja", "enabled":true},
                {"code":"vi", "enabled":true}
            ]
        }))
    }

    #[test]
    fn defaults_are_published_and_live_controls_cannot_widen_the_corpus()
    -> Result<(), PublishedSearchError> {
        let scope = snapshot_scope(&snapshot(), "project-a")?;
        let context = resolve_context(
            &scope,
            &live().map_err(PublishedSearchError::Controls)?,
            None,
            None,
            None,
        )?;
        assert_eq!(context.version_id(), "branch-main");
        assert_eq!(context.version_slug(), "main");
        assert_eq!(
            context.languages(),
            &BTreeSet::from(["en".to_owned(), "en-GB".to_owned()])
        );
        assert_eq!(context.result_limit(), 7);
        Ok(())
    }

    #[test]
    fn exact_selectors_and_unknown_fallback_never_select_live_drafts()
    -> Result<(), PublishedSearchError> {
        let scope = snapshot_scope(&snapshot(), "project-a")?;
        let live = live().map_err(PublishedSearchError::Controls)?;
        let exact = resolve_context(&scope, &live, Some("next"), Some("en-GB"), Some(1))?;
        assert_eq!(exact.version_id(), "branch-next");
        assert_eq!(exact.languages(), &BTreeSet::from(["en-GB".to_owned()]));
        for language in ["fr", "ja", "vi", "missing", "EN"] {
            let fallback =
                resolve_context(&scope, &live, Some("draft-only"), Some(language), None)?;
            assert_eq!(fallback.version_id(), "branch-main");
            assert_eq!(fallback.languages(), &BTreeSet::from(["en".to_owned()]));
        }
        Ok(())
    }

    #[test]
    fn a_disabled_or_missing_default_is_not_implicitly_reenabled()
    -> Result<(), PublishedSearchError> {
        let scope = snapshot_scope(&snapshot(), "project-a")?;
        let mut live = live().map_err(PublishedSearchError::Controls)?;
        live.languages.retain(|language| language.code != "en");
        let context = resolve_context(&scope, &live, None, Some("unknown"), None)?;
        assert!(context.languages().is_empty());
        let all = resolve_context(&scope, &live, None, None, None)?;
        assert_eq!(all.languages(), &BTreeSet::from(["en-GB".to_owned()]));
        live.languages[0].enabled = false;
        assert!(
            resolve_context(&scope, &live, None, None, None)?
                .languages()
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn result_limit_is_the_minimum_of_request_publication_and_live_caps()
    -> Result<(), PublishedSearchError> {
        let scope = snapshot_scope(&snapshot(), "project-a")?;
        let mut live = live().map_err(PublishedSearchError::Controls)?;
        assert_eq!(
            resolve_context(&scope, &live, None, None, Some(50))?.result_limit(),
            7
        );
        live.config = Some(json!({"search":{"maxResults":2}}));
        assert_eq!(
            resolve_context(&scope, &live, None, None, Some(50))?.result_limit(),
            2
        );
        assert_eq!(
            resolve_context(&scope, &live, None, None, None)?.result_limit(),
            2
        );
        assert_eq!(
            resolve_context(&scope, &live, None, None, Some(1))?.result_limit(),
            1
        );
        Ok(())
    }

    #[test]
    fn optional_controls_default_to_twelve_without_resetting_valid_caps()
    -> Result<(), PublishedSearchError> {
        for config in [
            Value::Null,
            json!({}),
            json!({"search":{}}),
            json!({"search":null}),
        ] {
            assert_eq!(result_cap(Some(&config))?, 12);
        }
        for cap in [json!(7), json!(7.0), json!(7e0)] {
            assert_eq!(result_cap(Some(&json!({"search":{"maxResults":cap}})))?, 7);
        }
        assert_eq!(
            result_cap(Some(
                &json!({"search":{"maxResults":3,"provider":"retired","filtersEnabled":false,"versionFilterEnabled":false,"aiAnswers":true}})
            ))?,
            3
        );
        Ok(())
    }

    #[test]
    fn malformed_known_controls_fail_closed_instead_of_raising_a_cap() {
        for config in [
            json!("invalid"),
            json!({"search":"invalid"}),
            json!({"search":{"maxResults":0}}),
            json!({"search":{"maxResults":51}}),
            json!({"search":{"maxResults":7.5}}),
            json!({"search":{"maxResults":-1}}),
            json!({"search":{"maxResults":"7"}}),
            json!({"search":{"maxResults":null}}),
        ] {
            assert!(result_cap(Some(&config)).is_err());
        }
    }

    #[test]
    fn resolver_uses_the_same_closed_snapshot_scope_validation_as_builds() {
        for (path, value) in [
            ("schema_version", json!(1)),
            ("schema_version", json!(3)),
            ("versions", json!([])),
            ("languages", json!([])),
            ("project", json!({"id":"foreign"})),
        ] {
            let mut snapshot = snapshot();
            snapshot[path] = value;
            assert!(snapshot_scope(&snapshot, "project-a").is_err());
        }
        let mut snapshot = snapshot();
        snapshot["versions"][1]["slug"] = json!("main");
        assert!(snapshot_scope(&snapshot, "project-a").is_err());
    }

    #[test]
    fn malformed_or_duplicate_live_language_evidence_is_not_accepted()
    -> Result<(), PublishedSearchError> {
        let mut controls = live().map_err(PublishedSearchError::Controls)?;
        controls.languages.push(LiveLanguage {
            code: "en".to_owned(),
            enabled: true,
        });
        assert!(live_languages(&controls).is_err());
        controls.languages.pop();
        controls.languages[0].code = " en ".to_owned();
        assert!(live_languages(&controls).is_err());
        Ok(())
    }
}
