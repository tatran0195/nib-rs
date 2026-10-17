//! Closed, typed search view of the versioned native publication contract.
//! Never guess missing visibility/version facts from draft rows or format 1.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::Value;

use crate::{
    SearchDocument, registry::RegistryError, text, validate_document, validate_identifier,
};

#[derive(Deserialize)]
struct Snapshot {
    #[serde(flatten)]
    scope: SnapshotScope,
    pages: Vec<Page>,
}

#[derive(Deserialize)]
pub(crate) struct SnapshotScope {
    schema_version: u16,
    pub project: ProjectBinding,
    pub languages: Vec<Language>,
    pub versions: Vec<Version>,
}

#[derive(Deserialize)]
pub(crate) struct ProjectBinding {
    pub id: String,
    // The full rendering config is intentionally opaque here. The publication
    // resolver separately parses and validates the known search-cap setting.
    pub config: Option<Value>,
}

#[derive(Deserialize)]
pub(crate) struct Language {
    pub id: String,
    pub code: String,
    pub is_default: bool,
    pub enabled: bool,
}

#[derive(Deserialize)]
pub(crate) struct Version {
    pub id: String,
    pub slug: String,
    pub is_default: bool,
}

impl SnapshotScope {
    fn check_binding(&self, expected_project_id: &str) -> Result<(), RegistryError> {
        if self.schema_version != 2 {
            return Err(RegistryError::Snapshot(
                "snapshot format 2 is required; republish without mutating historical snapshots",
            ));
        }
        if self.project.id != expected_project_id {
            return Err(RegistryError::Snapshot(
                "snapshot project does not match the selected project",
            ));
        }
        Ok(())
    }
}

pub(crate) fn snapshot_scope(
    snapshot: &Value,
    expected_project_id: &str,
) -> Result<SnapshotScope, RegistryError> {
    let scope = SnapshotScope::deserialize(snapshot)
        .map_err(|_| RegistryError::Snapshot("snapshot scope is missing or malformed"))?;
    scope.check_binding(expected_project_id)?;
    language_map(&scope.languages)?;
    version_map(&scope.versions)?;
    Ok(scope)
}

#[derive(Deserialize)]
struct Page {
    id: String,
    language_id: String,
    language_code: String,
    version_id: String,
    kind: PageKind,
    title: String,
    path: String,
    content: String,
    description: Option<String>,
    icon: Option<String>,
    hidden: bool,
    config: Option<PageConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "UPPERCASE")]
enum PageKind {
    Page,
    Group,
}

// Ignore unrelated rendering settings, but reject malformed known search
// settings. In particular a string-valued noindex must not default to false.
#[derive(Deserialize)]
struct PageConfig {
    seo: Option<Seo>,
    #[serde(rename = "sidebarTitle")]
    sidebar_title: Option<String>,
}

#[derive(Deserialize)]
struct Seo {
    #[serde(default)]
    noindex: bool,
}

pub(crate) fn snapshot_documents(
    snapshot: &Value,
    expected_project_id: &str,
) -> Result<Vec<SearchDocument>, RegistryError> {
    let input = Snapshot::deserialize(snapshot)
        .map_err(|_| RegistryError::Snapshot("snapshot search fields are missing or malformed"))?;
    input.scope.check_binding(expected_project_id)?;
    let languages = language_map(&input.scope.languages)?;
    let versions = version_map(&input.scope.versions)?;
    let mut ids = BTreeSet::new();
    let mut documents = Vec::new();
    for page in input.pages {
        validate_identifier(&page.id)?;
        if !ids.insert(page.id.clone()) {
            return Err(RegistryError::Snapshot(
                "snapshot contains duplicate page IDs",
            ));
        }
        let language = languages
            .get(page.language_id.as_str())
            .ok_or(RegistryError::Snapshot(
                "page language is outside its snapshot",
            ))?;
        if language.code != page.language_code {
            return Err(RegistryError::Snapshot(
                "page language ID and exact code disagree",
            ));
        }
        let version = versions
            .get(page.version_id.as_str())
            .ok_or(RegistryError::Snapshot(
                "page version is outside its snapshot",
            ))?;
        let noindex = page
            .config
            .as_ref()
            .and_then(|config| config.seo.as_ref())
            .is_some_and(|seo| seo.noindex);
        if matches!(page.kind, PageKind::Group) || page.hidden || noindex || !language.enabled {
            continue;
        }
        let document = SearchDocument {
            page_id: page.id,
            language: page.language_code,
            version_id: page.version_id,
            version_slug: version.slug.clone(),
            path: page.path,
            title: page.title,
            description: page.description.unwrap_or_default(),
            icon: page.icon,
            headings: text::headings(
                &page.content,
                page.config
                    .as_ref()
                    .and_then(|config| config.sidebar_title.as_deref()),
            ),
            content: page.content,
        };
        validate_document(&document)?;
        documents.push(document);
    }
    // Navigation-only (or wholly ineligible) publications deliberately produce
    // a valid empty generation, never a fallback to an older corpus.
    Ok(documents)
}

fn language_map(languages: &[Language]) -> Result<BTreeMap<&str, &Language>, RegistryError> {
    if languages
        .iter()
        .filter(|language| language.is_default)
        .count()
        != 1
        || languages
            .iter()
            .any(|language| language.is_default && !language.enabled)
    {
        return Err(RegistryError::Snapshot(
            "snapshot requires one enabled default language",
        ));
    }
    let mut map = BTreeMap::new();
    let mut codes = BTreeSet::new();
    for language in languages {
        validate_identifier(&language.id)?;
        validate_identifier(&language.code)?;
        if map.insert(language.id.as_str(), language).is_some()
            || !codes.insert(language.code.as_str())
        {
            return Err(RegistryError::Snapshot(
                "snapshot language IDs/codes are ambiguous",
            ));
        }
    }
    Ok(map)
}

fn version_map(versions: &[Version]) -> Result<BTreeMap<&str, &Version>, RegistryError> {
    if versions.iter().filter(|version| version.is_default).count() != 1 {
        return Err(RegistryError::Snapshot(
            "snapshot requires one default version",
        ));
    }
    let mut map = BTreeMap::new();
    let mut slugs = BTreeSet::new();
    for version in versions {
        validate_identifier(&version.id)?;
        validate_identifier(&version.slug)?;
        if map.insert(version.id.as_str(), version).is_some()
            || !slugs.insert(version.slug.as_str())
        {
            return Err(RegistryError::Snapshot(
                "snapshot version IDs/slugs are ambiguous",
            ));
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn page(id: &str) -> Value {
        json!({
            "id": id, "language_id":"lang-en", "language_code":"en", "version_id":"branch-main",
            "kind":"PAGE", "title":"Guide", "path":"guide", "content":"## Heading\nContent",
            "hidden":false, "description":"Summary", "icon":"book", "config":{"sidebarTitle":" Nav "}
        })
    }

    fn snapshot(pages: &[Value]) -> Value {
        json!({
            "schema_version":2, "project":{"id":"project-a"},
            "languages":[
                {"id":"lang-en", "code":"en", "is_default":true, "enabled":true},
                {"id":"lang-fr", "code":"fr", "is_default":false, "enabled":false}
            ],
            "versions":[
                {"id":"branch-main", "slug":"main", "is_default":true},
                {"id":"branch-v2", "slug":"v2", "is_default":false}
            ],
            "pages":pages
        })
    }

    #[test]
    fn extraction_preserves_versions_rendering_and_excludes_ineligible_pages()
    -> Result<(), RegistryError> {
        let mut hidden = page("hidden");
        hidden["hidden"] = json!(true);
        let mut noindex = page("noindex");
        noindex["config"] = json!({"seo":{"noindex":true}});
        let mut group = page("group");
        group["kind"] = json!("GROUP");
        let mut disabled = page("disabled");
        disabled["language_id"] = json!("lang-fr");
        disabled["language_code"] = json!("fr");
        let mut v2 = page("version-two");
        v2["version_id"] = json!("branch-v2");
        let documents = snapshot_documents(
            &snapshot(&[page("visible"), hidden, noindex, group, disabled, v2]),
            "project-a",
        )?;
        assert_eq!(documents.len(), 2);
        assert_eq!(documents[0].page_id, "visible");
        assert_eq!(documents[0].description, "Summary");
        assert_eq!(documents[0].icon.as_deref(), Some("book"));
        assert_eq!(documents[0].headings, "Nav Heading");
        assert_eq!(documents[1].version_id, "branch-v2");
        assert_eq!(documents[1].version_slug, "v2");
        Ok(())
    }

    #[test]
    fn navigation_only_snapshot_produces_an_empty_generation_corpus() -> Result<(), RegistryError> {
        let mut group = page("group");
        group["kind"] = json!("GROUP");
        assert_eq!(
            snapshot_documents(&snapshot(&[group]), "project-a")?.len(),
            0
        );
        let mut hidden = page("hidden");
        hidden["hidden"] = json!(true);
        assert_eq!(
            snapshot_documents(&snapshot(&[hidden]), "project-a")?.len(),
            0
        );
        Ok(())
    }

    #[test]
    fn extraction_rejects_legacy_unknown_or_ambiguous_scope_and_visibility() {
        for version in [json!(1), json!(3), Value::Null] {
            let mut input = snapshot(&[page("visible")]);
            input["schema_version"] = version;
            assert!(snapshot_documents(&input, "project-a").is_err());
        }
        assert!(snapshot_documents(&snapshot(&[page("visible")]), "other-project").is_err());
        for (field, value) in [
            ("language_code", json!("en-GB")),
            ("language_id", json!("foreign-language")),
            ("version_id", json!("foreign-version")),
            ("hidden", Value::Null),
            ("config", json!({"seo":{"noindex":"false"}})),
            ("kind", json!("UNKNOWN")),
        ] {
            let mut broken = page("broken");
            broken[field] = value;
            assert!(snapshot_documents(&snapshot(&[broken]), "project-a").is_err());
        }
        assert!(
            snapshot_documents(
                &snapshot(&[page("duplicate"), page("duplicate")]),
                "project-a"
            )
            .is_err()
        );
        let mut input = snapshot(&[]);
        input["versions"][1]["slug"] = json!("main");
        assert!(snapshot_documents(&input, "project-a").is_err());
        let mut input = snapshot(&[]);
        input["versions"][1]["is_default"] = json!(true);
        assert!(snapshot_documents(&input, "project-a").is_err());
        let mut input = snapshot(&[]);
        input["languages"][0]["enabled"] = json!(false);
        assert!(snapshot_documents(&input, "project-a").is_err());
        let mut input = snapshot(&[]);
        input["languages"][1]["code"] = json!("en");
        assert!(snapshot_documents(&input, "project-a").is_err());
    }
}
