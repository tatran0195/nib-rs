//! Real publication-composition proofs, sharing only the reader SQL fixtures.

use std::{collections::BTreeSet, env, error::Error, fs, path::PathBuf, time::Duration};

use axum::http::{HeaderMap, HeaderValue, header};
use nibleaf_search::{
    PublishedSearchError, PublishedSearchRequest, PublishedSearchResult, generation_directory,
    prepare_published_search,
    registry::{
        ClaimOutcome, SearchBuildRequest, build_or_reuse_claimed_generation,
        canonical_snapshot_sha256, claim_generation_build, load_claimed_snapshot_documents,
        promote_claimed_generation, register_generation_build,
    },
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::{FixtureProject, actor, add_member, create_page, create_reader_scope, fixture_project};

#[path = "query_execution.rs"]
mod execution_proof;

pub(super) async fn verify_execution(pool: &PgPool, suffix: &str) -> TestResult {
    execution_proof::verify(pool, suffix).await
}

#[path = "query_load_recovery.rs"]
mod load_proof;
pub(super) async fn verify_load(pool: &PgPool, suffix: &str) -> TestResult {
    load_proof::verify(pool, suffix).await
}

#[path = "object_distribution.rs"]
mod object_proof;
pub(super) async fn verify_object_distribution(pool: &PgPool, suffix: &str) -> TestResult {
    object_proof::verify(pool, suffix).await
}

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct Fixture {
    project: FixtureProject,
    suffix: String,
    root: PathBuf,
    allowed_page: String,
}

struct Publication {
    request: SearchBuildRequest,
    snapshot: Value,
}

pub(super) async fn verify(pool: &PgPool, suffix: &str) -> TestResult {
    let fixture = fixture(pool, suffix).await?;
    assert_concealment(pool, &fixture).await?;
    let first = publication(pool, &fixture, 1, "firstprobe", 3).await?;
    set_status(pool, &first.request.deployment_id, "READY").await?;
    assert_defaults_and_live_bounds(pool, &fixture).await?;
    assert_draft_default_changes_cannot_replace_published_defaults(pool, &fixture).await?;
    assert_one_read_snapshot(pool, &fixture).await?;
    let latest = assert_ready_selection(pool, &fixture, &first).await?;
    assert_broken_latest_never_falls_back(pool, &fixture, &latest).await?;
    assert_concealment_precedes_io_on_a_published_site(pool, &fixture).await?;
    assert_composed_workspace_and_reader_scope(pool, &fixture).await?;
    fs::remove_dir_all(&fixture.root)?;
    Ok(())
}

async fn fixture(pool: &PgPool, suffix: &str) -> TestResult<Fixture> {
    let project = fixture_project(pool, suffix, "published", "PUBLIC").await?;
    let allowed_page = format!("published-allowed-{suffix}");
    create_page(pool, &project, &allowed_page).await?;
    for (position, code) in [(1, "en-GB"), (2, "fr"), (3, "ja"), (4, "vi")] {
        sqlx::query(
            r#"INSERT INTO language (id,"projectId",code,label,direction,"isDefault",enabled,position,"createdAt","updatedAt")
               VALUES ($1,$2,$3,$3,'LTR',FALSE,TRUE,$4,NOW(),NOW())"#,
        )
        .bind(format!("published-language-{code}-{suffix}"))
        .bind(project.project_id.as_str())
        .bind(code)
        .bind(position)
        .execute(pool)
        .await?;
    }
    sqlx::query(
        r#"INSERT INTO branch (id,"projectId",name,"isDefault","createdAt","updatedAt")
           VALUES ($1,$2,'Next',FALSE,NOW(),NOW())"#,
    )
    .bind(format!("published-version-next-{suffix}"))
    .bind(project.project_id.as_str())
    .execute(pool)
    .await?;
    Ok(Fixture {
        project,
        suffix: suffix.to_owned(),
        root: env::temp_dir().join(format!("nibleaf-published-composition-{suffix}")),
        allowed_page,
    })
}

async fn assert_concealment(pool: &PgPool, fixture: &Fixture) -> TestResult {
    // An admitted but unpublished site is concealed without opening the
    // deliberately nonexistent root. So are taken-down and private sites.
    assert!(query(pool, fixture, "*", None, None, None).await?.is_none());
    for mode in ["WORKSPACE", "READERS"] {
        set_mode(pool, fixture, mode).await?;
        assert!(query(pool, fixture, "*", None, None, None).await?.is_none());
    }
    set_mode(pool, fixture, "PUBLIC").await?;
    sqlx::query("UPDATE project SET \"takedownAt\"=NOW() WHERE id=$1")
        .bind(fixture.project.project_id.as_str())
        .execute(pool)
        .await?;
    assert!(query(pool, fixture, "*", None, None, None).await?.is_none());
    sqlx::query("UPDATE project SET \"takedownAt\"=NULL WHERE id=$1")
        .bind(fixture.project.project_id.as_str())
        .execute(pool)
        .await?;
    Ok(())
}

async fn assert_defaults_and_live_bounds(pool: &PgPool, fixture: &Fixture) -> TestResult {
    set_cap(pool, fixture, 50).await?;
    let result = query(pool, fixture, "firstprobe", None, None, Some(50))
        .await?
        .ok_or("published site was not admitted")?;
    assert_eq!(result.publication_version, 1);
    assert_eq!(result.context.version_slug(), "main");
    assert_eq!(result.context.result_limit(), 3);
    assert_eq!(result.hits.len(), 3);
    assert_eq!(result.context.languages(), &codes(&["en", "en-GB", "fr"]));
    // The live ja/vi flags cannot expose a language excluded at capture or
    // absent from the immutable snapshot. Unknown selectors use its default.
    for language in ["ja", "vi", "unknown"] {
        let result = query(
            pool,
            fixture,
            "firstprobe",
            Some("draft-only"),
            Some(language),
            None,
        )
        .await?
        .ok_or("fallback was concealed")?;
        assert_eq!(result.context.version_slug(), "main");
        assert_eq!(result.context.languages(), &codes(&["en"]));
        assert!(result.hits.iter().all(|hit| hit.language == "en"));
    }
    let next = query(pool, fixture, "firstprobe", Some("next"), Some("en"), None)
        .await?
        .ok_or("exact version was concealed")?;
    assert_eq!(next.hits.len(), 1);
    assert!(next.hits[0].page_id.starts_with("published-next-"));
    set_cap(pool, fixture, 1).await?;
    let limited = query(pool, fixture, "firstprobe", None, None, Some(50))
        .await?
        .ok_or("live cap was concealed")?;
    assert_eq!(limited.context.result_limit(), 1);
    assert_eq!(limited.hits.len(), 1);
    set_cap(pool, fixture, 50).await?;
    set_language(pool, fixture, "fr", false).await?;
    let narrowed = query(pool, fixture, "firstprobe OR language:fr", None, None, None)
        .await?
        .ok_or("live disable was concealed")?;
    assert_eq!(narrowed.context.languages(), &codes(&["en", "en-GB"]));
    assert!(narrowed.hits.iter().all(|hit| hit.language != "fr"));
    set_language(pool, fixture, "en", false).await?;
    let no_default = query(pool, fixture, "firstprobe", None, Some("unknown"), None)
        .await?
        .ok_or("disabled default was concealed")?;
    assert!(no_default.context.languages().is_empty());
    assert_eq!(no_default.hits.len(), 0);
    set_language(pool, fixture, "en", true).await?;
    set_language(pool, fixture, "fr", true).await?;
    Ok(())
}

async fn assert_draft_default_changes_cannot_replace_published_defaults(
    pool: &PgPool,
    fixture: &Fixture,
) -> TestResult {
    for (table, next, original) in [
        (
            "branch",
            format!("published-version-next-{}", fixture.suffix),
            format!("reader-branch-{}", fixture.project.project_id.as_str()),
        ),
        (
            "language",
            format!("published-language-fr-{}", fixture.suffix),
            format!("reader-language-{}", fixture.project.project_id.as_str()),
        ),
    ] {
        // Only these two test-owned table names are interpolated; IDs remain
        // parameters. This is fixture mutation, not application dynamic SQL.
        let reset = format!("UPDATE {table} SET \"isDefault\"=FALSE WHERE \"projectId\"=$1");
        let select = format!("UPDATE {table} SET \"isDefault\"=TRUE WHERE id=$1");
        let mut transaction = pool.begin().await?;
        sqlx::query(&reset)
            .bind(fixture.project.project_id.as_str())
            .execute(&mut *transaction)
            .await?;
        sqlx::query(&select)
            .bind(&next)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        let result = query(
            pool,
            fixture,
            "firstprobe",
            Some("unknown"),
            Some("unknown"),
            None,
        )
        .await?
        .ok_or("changed drafts hid the published defaults")?;
        assert_eq!(result.context.version_slug(), "main");
        assert_eq!(result.context.languages(), &codes(&["en"]));
        let mut transaction = pool.begin().await?;
        sqlx::query(&reset)
            .bind(fixture.project.project_id.as_str())
            .execute(&mut *transaction)
            .await?;
        sqlx::query(&select)
            .bind(&original)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
    }
    Ok(())
}

async fn assert_one_read_snapshot(pool: &PgPool, fixture: &Fixture) -> TestResult {
    set_cap(pool, fixture, 3).await?;
    let mut blocker = pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    sqlx::query("LOCK TABLE rust_search_generation IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await?;
    let changing = async {
        // Admission can read project/session tables, but the selection query
        // must wait on this real PostgreSQL lock. Commit live changes only
        // after it has reached that barrier, then let selection continue.
        loop {
            let waiting: bool = sqlx::query_scalar(
                r"SELECT EXISTS(SELECT 1 FROM pg_stat_activity
                   WHERE wait_event_type='Lock' AND $1=ANY(pg_blocking_pids(pid))
                     AND query LIKE '%nibleaf:published-search-selection%')",
            )
            .bind(blocker_pid)
            .fetch_one(pool)
            .await?;
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        set_cap(pool, fixture, 1).await?;
        set_language(pool, fixture, "en-GB", false).await?;
        blocker.commit().await?;
        Ok::<(), Box<dyn Error>>(())
    };
    let searching = query(pool, fixture, "firstprobe", None, None, Some(50));
    // Structured database concurrency: the driver owns no spawned task, and
    // dropping the lock-owning transaction releases its database barrier.
    let (searched, changed) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(searching, changing)
    })
    .await?;
    changed?;
    let during = searched?.ok_or("read-snapshot request was concealed")?;
    assert_eq!(during.context.result_limit(), 3);
    assert!(during.context.languages().contains("en-GB"));
    let after = query(pool, fixture, "firstprobe", None, None, Some(50))
        .await?
        .ok_or("next read-snapshot request was concealed")?;
    assert_eq!(after.context.result_limit(), 1);
    assert!(!after.context.languages().contains("en-GB"));
    set_language(pool, fixture, "en-GB", true).await?;
    set_cap(pool, fixture, 50).await?;
    Ok(())
}

async fn assert_ready_selection(
    pool: &PgPool,
    fixture: &Fixture,
    first: &Publication,
) -> TestResult<Publication> {
    let second = publication(pool, fixture, 2, "secondprobe", 2).await?;
    assert_ne!(first.request.generation_id, second.request.generation_id);
    let (active, state): (Uuid, String) = sqlx::query_as(
        r"SELECT active.generation_id, generation.state
          FROM rust_search_active_generation AS active
          JOIN rust_search_generation AS generation ON generation.generation_id=$2
          WHERE active.project_id=$1",
    )
    .bind(fixture.project.project_id.as_str())
    .bind(first.request.generation_id)
    .fetch_one(pool)
    .await?;
    assert_eq!(active, second.request.generation_id);
    assert_eq!(state, "superseded");
    let gap = query(pool, fixture, "firstprobe", None, None, None)
        .await?
        .ok_or("promotion gap hid the previous READY publication")?;
    assert_eq!(gap.deployment_id, first.request.deployment_id);
    assert_eq!(gap.generation_id, first.request.generation_id);
    assert_ne!(gap.hits.len(), 0);
    set_status(pool, &second.request.deployment_id, "READY").await?;
    let complete = query(pool, fixture, "secondprobe", None, None, None)
        .await?
        .ok_or("new READY publication was concealed")?;
    assert_eq!(complete.deployment_id, second.request.deployment_id);
    assert_eq!(complete.publication_version, 2);
    assert_eq!(complete.context.result_limit(), 2);
    assert_eq!(
        query(pool, fixture, "firstprobe", None, None, None)
            .await?
            .ok_or("new READY publication was concealed")?
            .hits
            .len(),
        0
    );
    let third = publication(pool, fixture, 3, "thirdprobe", 1).await?;
    set_status(pool, &third.request.deployment_id, "FAILED").await?;
    let failed = query(pool, fixture, "secondprobe", None, None, None)
        .await?
        .ok_or("failed newer deployment hid the READY publication")?;
    assert_eq!(failed.deployment_id, second.request.deployment_id);
    Ok(second)
}

async fn assert_broken_latest_never_falls_back(
    pool: &PgPool,
    fixture: &Fixture,
    latest: &Publication,
) -> TestResult {
    let missing = format!("published-no-generation-{}", fixture.suffix);
    insert_deployment(pool, fixture, &missing, 4, &latest.snapshot).await?;
    set_status(pool, &missing, "READY").await?;
    assert!(matches!(
        query(pool, fixture, "secondprobe", None, None, None).await,
        Err(PublishedSearchError::Unavailable(_))
    ));
    sqlx::query("DELETE FROM deployment WHERE id=$1")
        .bind(&missing)
        .execute(pool)
        .await?;
    let mut changed = latest.snapshot.clone();
    changed["project"]["config"]["search"]["maxResults"] = json!(50);
    sqlx::query("UPDATE deployment SET snapshot=$2 WHERE id=$1")
        .bind(&latest.request.deployment_id)
        .bind(changed)
        .execute(pool)
        .await?;
    assert!(matches!(
        query(pool, fixture, "secondprobe", None, None, None).await,
        Err(PublishedSearchError::Unavailable(_))
    ));
    sqlx::query("UPDATE deployment SET snapshot=$2 WHERE id=$1")
        .bind(&latest.request.deployment_id)
        .bind(&latest.snapshot)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE rust_search_generation SET documents_sha256=$2 WHERE generation_id=$1")
        .bind(latest.request.generation_id)
        .bind("0".repeat(64))
        .execute(pool)
        .await?;
    assert!(matches!(
        query(pool, fixture, "secondprobe", None, None, None).await,
        Err(PublishedSearchError::Unavailable(_))
    ));
    let path = generation_directory(
        &fixture.root,
        fixture.project.project_id.as_str(),
        latest.request.generation_id,
    );
    let manifest_path = path.join("manifest.json");
    let original = fs::read(&manifest_path)?;
    let mut manifest: Value = serde_json::from_slice(&original)?;
    sqlx::query("UPDATE rust_search_generation SET documents_sha256=$2 WHERE generation_id=$1")
        .bind(latest.request.generation_id)
        .bind(
            manifest["documents_sha256"]
                .as_str()
                .ok_or("manifest digest missing")?,
        )
        .execute(pool)
        .await?;
    sqlx::query("UPDATE rust_search_generation SET state='failed', last_error='test_only_failure' WHERE generation_id=$1")
        .bind(latest.request.generation_id).execute(pool).await?;
    assert!(matches!(
        query(pool, fixture, "secondprobe", None, None, None).await,
        Err(PublishedSearchError::Unavailable(_))
    ));
    sqlx::query("UPDATE rust_search_generation SET state='superseded', last_error=NULL WHERE generation_id=$1")
        .bind(latest.request.generation_id).execute(pool).await?;
    manifest["runtime"]["lindera"] = json!("2.3.4");
    fs::write(&manifest_path, serde_json::to_vec(&manifest)?)?;
    assert!(matches!(
        query(pool, fixture, "secondprobe", None, None, None).await,
        Err(PublishedSearchError::Search(_))
    ));
    fs::write(&manifest_path, original)?;
    let moved = path.with_extension("temporarily-missing");
    fs::rename(&path, &moved)?;
    assert!(matches!(
        query(pool, fixture, "secondprobe", None, None, None).await,
        Err(PublishedSearchError::Search(_))
    ));
    fs::rename(moved, path)?;
    sqlx::query(r#"UPDATE project SET config='{"search":{"maxResults":0}}'::jsonb WHERE id=$1"#)
        .bind(fixture.project.project_id.as_str())
        .execute(pool)
        .await?;
    assert!(matches!(
        query(pool, fixture, "secondprobe", None, None, None).await,
        Err(PublishedSearchError::Unavailable(_))
    ));
    set_cap(pool, fixture, 50).await?;
    Ok(())
}

async fn assert_concealment_precedes_io_on_a_published_site(
    pool: &PgPool,
    fixture: &Fixture,
) -> TestResult {
    let missing_root = fixture.root.join("must-not-be-opened");
    let request = PublishedSearchRequest {
        query: "*",
        version: None,
        language: None,
        limit: None,
    };
    for mode in ["WORKSPACE", "READERS"] {
        set_mode(pool, fixture, mode).await?;
        assert!(
            search_published_site(
                pool,
                &missing_root,
                &fixture.project.project_id,
                &HeaderMap::new(),
                None,
                request
            )
            .await?
            .is_none()
        );
    }
    set_mode(pool, fixture, "PUBLIC").await?;
    sqlx::query("UPDATE project SET \"takedownAt\"=NOW() WHERE id=$1")
        .bind(fixture.project.project_id.as_str())
        .execute(pool)
        .await?;
    assert!(
        search_published_site(
            pool,
            &missing_root,
            &fixture.project.project_id,
            &HeaderMap::new(),
            None,
            request
        )
        .await?
        .is_none()
    );
    sqlx::query("UPDATE project SET \"takedownAt\"=NULL WHERE id=$1")
        .bind(fixture.project.project_id.as_str())
        .execute(pool)
        .await?;
    Ok(())
}

async fn assert_composed_workspace_and_reader_scope(
    pool: &PgPool,
    fixture: &Fixture,
) -> TestResult {
    set_mode(pool, fixture, "WORKSPACE").await?;
    assert!(
        query(pool, fixture, "secondprobe", None, None, None)
            .await?
            .is_none()
    );
    add_member(
        pool,
        &fixture.project.organization_id,
        "test-owner",
        &format!("published-member-{}", fixture.suffix),
    )
    .await?;
    let owner = actor("test-owner", "test-owner-session")?;
    let request = PublishedSearchRequest {
        query: "secondprobe",
        version: None,
        language: None,
        limit: Some(50),
    };
    let member = search_published_site(
        pool,
        &fixture.root,
        &fixture.project.project_id,
        &HeaderMap::new(),
        Some(&owner),
        request,
    )
    .await?
    .ok_or("workspace member was concealed")?;
    assert_eq!(member.hits.len(), 2);
    assert_composed_reader_scope(pool, fixture, &owner).await
}

async fn assert_composed_reader_scope(
    pool: &PgPool,
    fixture: &Fixture,
    owner: &nibleaf_core::Actor,
) -> TestResult {
    let request = PublishedSearchRequest {
        query: "secondprobe",
        version: None,
        language: None,
        limit: Some(50),
    };
    set_mode(pool, fixture, "READERS").await?;
    let reader_id = format!("published-reader-{}", fixture.suffix);
    let audience_id = format!("published-audience-{}", fixture.suffix);
    let token = format!("published-token-{}", fixture.suffix);
    create_reader_scope(
        pool,
        &fixture.project.project_id,
        &reader_id,
        &audience_id,
        &fixture.allowed_page,
        &token,
    )
    .await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!(
            "nibleaf_reader_{}={token}",
            fixture.project.project_id.as_str()
        ))?,
    );
    let before: time::PrimitiveDateTime =
        sqlx::query_scalar("SELECT \"lastUsedAt\" FROM reader_session WHERE \"readerId\"=$1")
            .bind(&reader_id)
            .fetch_one(pool)
            .await?;
    let result = search_published_site(
        pool,
        &fixture.root,
        &fixture.project.project_id,
        &headers,
        None,
        PublishedSearchRequest {
            limit: Some(1),
            ..request
        },
    )
    .await?
    .ok_or("reader was concealed")?;
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].page_id, fixture.allowed_page);
    sqlx::query("DELETE FROM audience_grant WHERE \"audienceId\"=$1")
        .bind(&audience_id)
        .execute(pool)
        .await?;
    let zero = search_published_site(
        pool,
        &fixture.root,
        &fixture.project.project_id,
        &headers,
        None,
        request,
    )
    .await?
    .ok_or("zero-grant reader was concealed")?;
    assert_eq!(zero.hits.len(), 0);
    let after: time::PrimitiveDateTime =
        sqlx::query_scalar("SELECT \"lastUsedAt\" FROM reader_session WHERE \"readerId\"=$1")
            .bind(&reader_id)
            .fetch_one(pool)
            .await?;
    assert_eq!(before, after);
    sqlx::query("UPDATE reader_session SET \"revokedAt\"=NOW() WHERE \"readerId\"=$1")
        .bind(&reader_id)
        .execute(pool)
        .await?;
    assert!(
        search_published_site(
            pool,
            &fixture.root,
            &fixture.project.project_id,
            &headers,
            None,
            request
        )
        .await?
        .is_none()
    );
    let override_result = search_published_site(
        pool,
        &fixture.root,
        &fixture.project.project_id,
        &headers,
        Some(owner),
        request,
    )
    .await?
    .ok_or("workspace override was concealed")?;
    assert_eq!(override_result.hits.len(), 2);
    Ok(())
}

async fn publication(
    pool: &PgPool,
    fixture: &Fixture,
    version: i32,
    marker: &str,
    cap: usize,
) -> TestResult<Publication> {
    let deployment_id = format!("published-deployment-{version}-{}", fixture.suffix);
    let snapshot = snapshot(fixture, marker, cap);
    insert_deployment(pool, fixture, &deployment_id, version, &snapshot).await?;
    let mut transaction = pool.begin().await?;
    let request = register_generation_build(
        &mut transaction,
        &fixture.project.organization_id,
        fixture.project.project_id.as_str(),
        &deployment_id,
        &canonical_snapshot_sha256(&snapshot),
    )
    .await?;
    transaction.commit().await?;
    let ClaimOutcome::Claimed(claim) = claim_generation_build(
        pool,
        &request,
        Some(&fixture.project.organization_id),
        "published-composition-test",
        Duration::from_secs(30),
    )
    .await?
    else {
        return Err("new publication was unexpectedly already ready".into());
    };
    let documents = load_claimed_snapshot_documents(pool, &claim).await?;
    let manifest = tokio::task::block_in_place(|| {
        build_or_reuse_claimed_generation(&fixture.root, &claim, &documents)
    })?;
    promote_claimed_generation(pool, &claim, &manifest).await?;
    Ok(Publication { request, snapshot })
}

async fn insert_deployment(
    pool: &PgPool,
    fixture: &Fixture,
    id: &str,
    version: i32,
    snapshot: &Value,
) -> TestResult {
    sqlx::query(
        r#"INSERT INTO deployment (id,"projectId",version,status,snapshot,"pagesCount","createdAt")
           VALUES ($1,$2,$3,'PENDING', $4,6,NOW())"#,
    )
    .bind(id)
    .bind(fixture.project.project_id.as_str())
    .bind(version)
    .bind(snapshot)
    .execute(pool)
    .await?;
    Ok(())
}

fn snapshot(fixture: &Fixture, marker: &str, cap: usize) -> Value {
    let project = fixture.project.project_id.as_str();
    let en_id = format!("reader-language-{project}");
    let gb_id = format!("published-language-en-GB-{}", fixture.suffix);
    let fr_id = format!("published-language-fr-{}", fixture.suffix);
    let ja_id = format!("published-language-ja-{}", fixture.suffix);
    let main = format!("reader-branch-{project}");
    let next = format!("published-version-next-{}", fixture.suffix);
    json!({
        "schema_version":2,
        "project":{"id":project, "config":{"search":{"maxResults":cap, "filtersEnabled":false,"versionFilterEnabled":false}}},
        "languages":[
            {"id":en_id,"code":"en","is_default":true,"enabled":true},
            {"id":gb_id,"code":"en-GB","is_default":false,"enabled":true},
            {"id":fr_id,"code":"fr","is_default":false,"enabled":true},
            {"id":ja_id,"code":"ja","is_default":false,"enabled":false}
        ],
        "versions":[
            {"id":main,"slug":"main","is_default":true},
            {"id":next,"slug":"next","is_default":false}
        ],
        "pages":[
            page(&fixture.allowed_page, &en_id, "en", &main, marker, false),
            page(&format!("published-en-{}",fixture.suffix), &en_id, "en", &main, marker, true),
            page(&format!("published-gb-{}",fixture.suffix), &gb_id, "en-GB", &main, marker, true),
            page(&format!("published-fr-{}",fixture.suffix), &fr_id, "fr", &main, marker, true),
            page(&format!("published-ja-{}",fixture.suffix), &ja_id, "ja", &main, marker, true),
            page(&format!("published-next-{}",fixture.suffix), &en_id, "en", &next, marker, true)
        ]
    })
}

fn page(
    id: &str,
    language_id: &str,
    code: &str,
    version_id: &str,
    marker: &str,
    title_match: bool,
) -> Value {
    json!({
        "id":id,"language_id":language_id,"language_code":code,"version_id":version_id,
        "kind":"PAGE", "title":if title_match {marker} else {"Unrelated title"},
        "path":id,"content":format!("Read **{marker}** guide."),"description":"Frozen summary",
        "icon":"book","hidden":false,"config":null
    })
}

// The test's explicitly multithreaded Tokio runtime owns this synchronous
// phase with block_in_place, not a spawned/detached request task. Production
// HTTP wiring must supply a bounded, shutdown-owned executor before exposure.
async fn search_published_site(
    pool: &PgPool,
    root: &std::path::Path,
    project_id: &nibleaf_core::ProjectId,
    headers: &HeaderMap,
    actor: Option<&nibleaf_core::Actor>,
    request: PublishedSearchRequest<'_>,
) -> Result<Option<PublishedSearchResult>, PublishedSearchError> {
    let Some(prepared) =
        prepare_published_search(pool, project_id, headers, actor, request).await?
    else {
        return Ok(None);
    };
    tokio::task::block_in_place(|| prepared.execute(root)).map(Some)
}

async fn query(
    pool: &PgPool,
    fixture: &Fixture,
    query: &str,
    version: Option<&str>,
    language: Option<&str>,
    limit: Option<usize>,
) -> Result<Option<PublishedSearchResult>, PublishedSearchError> {
    search_published_site(
        pool,
        &fixture.root,
        &fixture.project.project_id,
        &HeaderMap::new(),
        None,
        PublishedSearchRequest {
            query,
            version,
            language,
            limit,
        },
    )
    .await
}

async fn set_mode(pool: &PgPool, fixture: &Fixture, mode: &str) -> TestResult {
    sqlx::query("UPDATE project SET \"accessMode\"=$2::\"ProjectAccessMode\" WHERE id=$1")
        .bind(fixture.project.project_id.as_str())
        .bind(mode)
        .execute(pool)
        .await?;
    Ok(())
}

async fn set_status(pool: &PgPool, id: &str, status: &str) -> TestResult {
    sqlx::query("UPDATE deployment SET status=$2::\"DeploymentStatus\" WHERE id=$1")
        .bind(id)
        .bind(status)
        .execute(pool)
        .await?;
    Ok(())
}

async fn set_cap(pool: &PgPool, fixture: &Fixture, cap: usize) -> TestResult {
    sqlx::query("UPDATE project SET config=$2 WHERE id=$1")
        .bind(fixture.project.project_id.as_str())
        .bind(json!({"search":{"maxResults":cap}}))
        .execute(pool)
        .await?;
    Ok(())
}

async fn set_language(
    pool: &PgPool,
    fixture: &Fixture,
    language: &str,
    enabled: bool,
) -> TestResult {
    sqlx::query("UPDATE language SET enabled=$3 WHERE \"projectId\"=$1 AND code=$2")
        .bind(fixture.project.project_id.as_str())
        .bind(language)
        .bind(enabled)
        .execute(pool)
        .await?;
    Ok(())
}

fn codes(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}
