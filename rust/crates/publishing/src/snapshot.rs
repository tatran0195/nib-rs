//! Version-2 publication capture. Project chrome, every branch/language, and
//! page metadata are read at one PostgreSQL statement snapshot, not stitched
//! together from independently changing READ COMMITTED query results.

use std::collections::{BTreeMap, BTreeSet};

use nibleaf_core::AppError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Postgres, Transaction, types::Json};

const MAX_SNAPSHOT_TEXT_BYTES: usize = 1_000_000;

#[derive(Deserialize)]
struct CaptureInput {
    project: SnapshotProject,
    languages: Vec<SnapshotLanguage>,
    branches: Vec<SnapshotBranch>,
    pages: Vec<SnapshotPage>,
}

#[derive(Deserialize, Serialize)]
struct SnapshotProject {
    id: String,
    name: String,
    slug: String,
    description: Option<String>,
    config: Option<Value>,
}

#[derive(Deserialize, Serialize)]
struct SnapshotLanguage {
    id: String,
    code: String,
    label: String,
    direction: String,
    is_default: bool,
    enabled: bool,
    position: i32,
    config: Option<Value>,
}

#[derive(Deserialize)]
struct SnapshotBranch {
    id: String,
    name: String,
    is_default: bool,
}

#[derive(Serialize)]
struct SnapshotVersion {
    id: String,
    name: String,
    slug: String,
    is_default: bool,
}

#[derive(Deserialize, Serialize)]
struct SnapshotPage {
    id: String,
    language_id: String,
    language_code: String,
    version_id: String,
    parent_id: Option<String>,
    kind: String,
    title: String,
    slug: String,
    path: String,
    description: Option<String>,
    icon: Option<String>,
    content: String,
    config: Option<Value>,
    hidden: bool,
    position: i32,
    translation_key: Option<String>,
    revision: i64,
    // Legacy FKs prove existence but not same-project binding. Do not silently
    // drop a corrupt/cross-project row with a scoped INNER JOIN; reject capture.
    #[serde(skip_serializing)]
    language_project_id: String,
    #[serde(skip_serializing)]
    branch_project_id: String,
}

async fn capture_inputs(
    transaction: &mut Transaction<'_, Postgres>,
    project_id: &str,
) -> Result<CaptureInput, AppError> {
    // Serialize with live project updates before taking the input statement's
    // snapshot. Keep the publication advisory lock in the calling use case.
    // REPEATABLE READ before that advisory lock would take a stale MVCC view
    // while waiting for a competing publication, so capture uses one statement
    // at READ COMMITTED instead.
    sqlx::query_scalar::<_, String>("SELECT id FROM project WHERE id = $1 FOR SHARE")
        .bind(project_id)
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or_else(|| AppError::not_found("project"))?;
    let Json(input) = sqlx::query_scalar::<_, Json<CaptureInput>>(
        r#"
        SELECT jsonb_build_object(
          'project', jsonb_build_object(
            'id', project.id, 'name', project.name, 'slug', project.slug,
            'description', project.description, 'config', project.config
          ),
          'languages', (
            SELECT COALESCE(jsonb_agg(row ORDER BY row.position, row.id), '[]'::jsonb)
            FROM (
              SELECT id, code, label, direction::text AS direction,
                     "isDefault" AS is_default, enabled, position, config
              FROM language WHERE "projectId" = $1
            ) AS row
          ),
          'branches', (
            SELECT COALESCE(jsonb_agg(row ORDER BY row.is_default DESC, row.created_at, row.id), '[]'::jsonb)
            FROM (
              SELECT id, name, "isDefault" AS is_default, "createdAt" AS created_at
              FROM branch WHERE "projectId" = $1
            ) AS row
          ),
          'pages', (
            SELECT COALESCE(jsonb_agg(row ORDER BY row.version_id, row.language_id, row.position, row.id), '[]'::jsonb)
            FROM (
              SELECT page.id, page."languageId" AS language_id, language.code AS language_code,
                     page."branchId" AS version_id, page."parentId" AS parent_id,
                     page.kind::text AS kind, page.title, page.slug, page.path,
                     page.description, page.icon, page.content, page.config,
                     page.hidden, page.position, page."translationKey" AS translation_key,
                     page.revision, language."projectId" AS language_project_id,
                     branch."projectId" AS branch_project_id
              FROM page
              INNER JOIN language ON language.id = page."languageId"
              INNER JOIN branch ON branch.id = page."branchId"
              WHERE page."projectId" = $1
            ) AS row
          )
        )
        FROM project WHERE id = $1
        "#,
    )
    .bind(project_id)
    .fetch_one(&mut **transaction)
    .await?;
    Ok(input)
}

pub(super) async fn capture_snapshot(
    transaction: &mut Transaction<'_, Postgres>,
    project_id: &str,
) -> Result<Value, AppError> {
    let mut input = capture_inputs(transaction, project_id).await?;
    if input
        .languages
        .iter()
        .filter(|language| language.is_default)
        .count()
        != 1
        || input
            .languages
            .iter()
            .any(|language| language.is_default && !language.enabled)
    {
        return Err(AppError::conflict(
            "project must have exactly one enabled default language",
        ));
    }
    if input
        .branches
        .iter()
        .filter(|branch| branch.is_default)
        .count()
        != 1
    {
        return Err(AppError::conflict(
            "project must have exactly one default branch",
        ));
    }
    if input
        .pages
        .iter()
        .any(|page| page.language_project_id != project_id || page.branch_project_id != project_id)
    {
        return Err(AppError::conflict(
            "page language or branch belongs to another project",
        ));
    }
    let mut used = BTreeSet::new();
    let versions: Vec<SnapshotVersion> = input
        .branches
        .into_iter()
        .map(|branch| SnapshotVersion {
            slug: unique_version_slug(&branch.name, &branch.id, &mut used),
            id: branch.id,
            name: branch.name,
            is_default: branch.is_default,
        })
        .collect();
    let variables = variables_from_config(input.project.config.as_ref());
    for page in &mut input.pages {
        page.title = interpolate_variables(&page.title, &variables)?;
        page.content = interpolate_variables(&page.content, &variables)?;
        page.description = page
            .description
            .as_deref()
            .map(|value| interpolate_variables(value, &variables))
            .transpose()?;
    }
    Ok(serde_json::json!({
        "schema_version": 2,
        "project": input.project,
        "languages": input.languages,
        "versions": versions,
        // Disabled, hidden, and noindex pages are retained as publication
        // facts, not search results. Extraction enforces corpus eligibility.
        "pages": input.pages,
    }))
}

// Deliberate compatibility with packages/shared/src/site.ts::versionSlug and
// uniqueVersionSlug, including collision suffixes and the opaque-ID fallback.
fn version_slug(name: &str, fallback: &str) -> String {
    let mut slug = String::new();
    let mut separator_pending = false;
    for character in name.trim().to_lowercase().chars() {
        if character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || matches!(character, '.' | '_' | '-')
        {
            if separator_pending && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character);
            separator_pending = false;
        } else if !slug.is_empty() {
            separator_pending = true;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        fallback.to_owned()
    } else {
        slug.to_owned()
    }
}

fn unique_version_slug(name: &str, fallback: &str, used: &mut BTreeSet<String>) -> String {
    let base = version_slug(name, fallback);
    let mut candidate = base.clone();
    if used.contains(&candidate) {
        let suffix: String = version_slug(fallback, fallback).chars().take(12).collect();
        candidate = format!(
            "{base}-{}",
            if suffix.is_empty() {
                "version"
            } else {
                &suffix
            }
        );
    }
    let mut n = 2;
    while used.contains(&candidate) {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    used.insert(candidate.clone());
    candidate
}

fn variables_from_config(config: Option<&Value>) -> BTreeMap<&str, &str> {
    let mut variables = BTreeMap::new();
    if let Some(list) = config
        .and_then(|config| config.get("variables"))
        .and_then(Value::as_array)
    {
        for item in list {
            if let (Some(key), Some(value)) = (
                item.get("key").and_then(Value::as_str),
                item.get("value").and_then(Value::as_str),
            ) && !key.is_empty()
            {
                variables.insert(key, value);
            }
        }
    }
    variables
}

fn append_bounded(output: &mut String, value: &str) -> Result<(), AppError> {
    if value.len() > MAX_SNAPSHOT_TEXT_BYTES.saturating_sub(output.len()) {
        return Err(AppError::validation(
            "published text exceeds its byte bound after variable substitution",
        ));
    }
    output.push_str(value);
    Ok(())
}

fn interpolate_variables(text: &str, variables: &BTreeMap<&str, &str>) -> Result<String, AppError> {
    let mut output = String::new();
    let mut remaining = text;
    while let Some(open) = remaining.find("{{") {
        append_bounded(&mut output, &remaining[..open])?;
        remaining = &remaining[open..];
        let Some(close) = remaining[2..].find("}}").map(|index| index + 2) else {
            break;
        };
        let key = remaining[2..close].trim();
        if !key.is_empty()
            && key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        {
            append_bounded(
                &mut output,
                variables
                    .get(key)
                    .copied()
                    .unwrap_or(&remaining[..close + 2]),
            )?;
            remaining = &remaining[close + 2..];
        } else {
            // Advance only the opening pair so an inner valid token (as in
            // "{{ bad {{product}} }}") retains the legacy regex behavior.
            append_bounded(&mut output, "{{")?;
            remaining = &remaining[2..];
        }
    }
    append_bounded(&mut output, remaining)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_slugs_match_legacy_ascii_fallback_and_collision_rules() {
        assert_eq!(version_slug(" Release / v1.2_β ", "id"), "release-v1.2_");
        assert_eq!(version_slug(" --- ", "branch-opaque"), "branch-opaque");
        assert_eq!(version_slug("日本語", "branch-opaque"), "branch-opaque");
        assert_eq!(version_slug("İ", "id"), "i");
        let mut used = BTreeSet::new();
        assert_eq!(unique_version_slug("main", "a", &mut used), "main");
        assert_eq!(
            unique_version_slug("Main", "branch-123456789012345", &mut used),
            "main-branch-12345"
        );
        assert_eq!(
            unique_version_slug("main", "branch-123456789012345", &mut used),
            "main-2"
        );
    }

    #[test]
    fn variables_are_frozen_without_recursive_or_unbounded_expansion() -> Result<(), AppError> {
        let config = serde_json::json!({"variables":[
            {"key":"product", "value":"old"}, {"key":"product", "value":"Nibleaf"},
            {"key":"nested", "value":"{{product}}"}, {"key":"invalid", "value":3}
        ]});
        let variables = variables_from_config(Some(&config));
        assert_eq!(
            interpolate_variables(
                "{{ product }} {{nested}} {{missing}} {{invalid}}",
                &variables
            )?,
            "Nibleaf {{product}} {{missing}} {{invalid}}"
        );
        assert_eq!(
            interpolate_variables("{{ bad {{product}} }}", &variables)?,
            "{{ bad Nibleaf }}"
        );
        assert_eq!(
            interpolate_variables("unclosed {{product", &variables)?,
            "unclosed {{product"
        );
        let huge = "x".repeat(MAX_SNAPSHOT_TEXT_BYTES);
        let variables = BTreeMap::from([("large", huge.as_str())]);
        assert!(interpolate_variables("{{large}}!", &variables).is_err());
        Ok(())
    }
}
