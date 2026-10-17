#![forbid(unsafe_code)]

//! Disposable PostgreSQL authorization proof for the future public-search gate.
//! The integration harness applies the legacy reader schema before this test.

use std::{collections::BTreeSet, env, fs, path::Path};

use axum::http::{HeaderMap, HeaderValue, header};
use nibleaf_core::{Actor, ProjectId, UserId};
use nibleaf_search::{
    ProjectIndexGeneration, PublishedSearchAdmission, PublishedViewerKind, SearchDocument,
    SearchHit, admit_published_search, build_generation, search_admitted_generation,
};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

#[path = "support/published_composition.rs"]
mod published_composition;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn object_publication_and_replica_lifecycles_are_fenced_in_postgres()
-> Result<(), Box<dyn std::error::Error>> {
    if env::var("NIBLEAF_SEARCH_OBJECT_DISTRIBUTION_E2E").is_err() {
        return Ok(());
    }
    let pool = PgPool::connect(&env::var("NIBLEAF_TEST_DATABASE_URL")?).await?;
    let suffix = Uuid::now_v7().simple().to_string();
    published_composition::verify_object_distribution(&pool, &suffix).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_query_load_and_offline_recovery_are_repeatable()
-> Result<(), Box<dyn std::error::Error>> {
    if env::var("NIBLEAF_SEARCH_LOAD_E2E").is_err() {
        return Ok(());
    }
    let pool = PgPool::connect(&env::var("NIBLEAF_TEST_DATABASE_URL")?).await?;
    let suffix = Uuid::now_v7().simple().to_string();
    published_composition::verify_load(&pool, &suffix).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounded_query_service_owns_database_and_cpu_lifecycles()
-> Result<(), Box<dyn std::error::Error>> {
    if env::var("NIBLEAF_SEARCH_QUERY_EXECUTION_E2E").is_err() {
        return Ok(());
    }
    let pool = PgPool::connect(&env::var("NIBLEAF_TEST_DATABASE_URL")?).await?;
    let suffix = Uuid::now_v7().simple().to_string();
    published_composition::verify_execution(&pool, &suffix).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn published_search_uses_one_ready_snapshot_with_live_restrictions()
-> Result<(), Box<dyn std::error::Error>> {
    if env::var("NIBLEAF_PUBLISHED_SEARCH_E2E").is_err() {
        return Ok(());
    }
    let pool = PgPool::connect(&env::var("NIBLEAF_TEST_DATABASE_URL")?).await?;
    let suffix = Uuid::now_v7().simple().to_string();
    published_composition::verify(&pool, &suffix).await
}

#[tokio::test]
async fn public_workspace_and_reader_admission_stays_project_scoped_and_fail_closed()
-> Result<(), Box<dyn std::error::Error>> {
    if env::var("NIBLEAF_PUBLIC_READER_ADMISSION_E2E").is_err() {
        return Ok(());
    }
    let pool = PgPool::connect(&env::var("NIBLEAF_TEST_DATABASE_URL")?).await?;
    let suffix = Uuid::now_v7().simple().to_string();
    assert_public_and_workspace_admission(&pool, &suffix).await?;
    assert_reader_admission(&pool, &suffix).await?;
    Ok(())
}

async fn assert_public_and_workspace_admission(
    pool: &PgPool,
    suffix: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let public = fixture_project(pool, suffix, "public", "PUBLIC").await?;
    let workspace = fixture_project(pool, suffix, "workspace", "WORKSPACE").await?;
    let owner = actor("test-owner", "test-owner-session")?;

    let anonymous_headers = HeaderMap::new();
    let public_admission =
        admit_published_search(pool, &public.project_id, &anonymous_headers, None)
            .await?
            .ok_or("public site was not admitted")?;
    assert_eq!(public_admission.kind(), PublishedViewerKind::Public);
    assert_eq!(public_admission.allowed_page_ids(), None);

    sqlx::query("UPDATE project SET \"takedownAt\"=NOW() WHERE id=$1")
        .bind(public.project_id.as_str())
        .execute(pool)
        .await?;
    assert!(
        admit_published_search(pool, &public.project_id, &anonymous_headers, None)
            .await?
            .is_none()
    );

    assert!(
        admit_published_search(pool, &workspace.project_id, &anonymous_headers, None)
            .await?
            .is_none()
    );
    add_member(
        pool,
        &workspace.organization_id,
        "test-owner",
        &format!("workspace-member-{suffix}"),
    )
    .await?;
    let workspace_admission = admit_published_search(
        pool,
        &workspace.project_id,
        &anonymous_headers,
        Some(&owner),
    )
    .await?
    .ok_or("workspace member was not admitted")?;
    assert_eq!(workspace_admission.kind(), PublishedViewerKind::Workspace);
    assert_eq!(workspace_admission.allowed_page_ids(), None);
    let outsider = actor("test-outsider", "test-outsider-session")?;
    assert!(
        admit_published_search(
            pool,
            &workspace.project_id,
            &anonymous_headers,
            Some(&outsider)
        )
        .await?
        .is_none()
    );

    Ok(())
}

async fn assert_reader_admission(
    pool: &PgPool,
    suffix: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let readers = fixture_project(pool, suffix, "readers", "READERS").await?;
    let reader_page_id = format!("reader-page-{suffix}");
    create_page(pool, &readers, &reader_page_id).await?;
    let reader_id = format!("reader-{suffix}");
    let audience_id = format!("audience-{suffix}");
    let reader_token = format!("reader-token-{suffix}");
    create_reader_scope(
        pool,
        &readers.project_id,
        &reader_id,
        &audience_id,
        &reader_page_id,
        &reader_token,
    )
    .await?;
    let mut reader_headers = HeaderMap::new();
    reader_headers.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!(
            "nibleaf_reader_{}={reader_token}",
            readers.project_id.as_str()
        ))?,
    );
    let before_last_used: time::PrimitiveDateTime =
        sqlx::query_scalar("SELECT \"lastUsedAt\" FROM reader_session WHERE \"readerId\"=$1")
            .bind(&reader_id)
            .fetch_one(pool)
            .await?;
    let reader_admission = admit_published_search(pool, &readers.project_id, &reader_headers, None)
        .await?
        .ok_or("reader was not admitted")?;
    assert_eq!(reader_admission.kind(), PublishedViewerKind::Reader);
    assert_eq!(
        reader_admission.allowed_page_ids(),
        Some(&BTreeSet::from([reader_page_id.clone()]))
    );
    assert_eq!(reader_admission.project_id(), readers.project_id.as_str());
    let root = env::temp_dir().join(format!("nibleaf-reader-query-{suffix}"));
    let generation = build_reader_generation(&root, &readers.project_id, &reader_page_id).await?;
    let hits = reader_query(
        &generation,
        &reader_admission,
        "branch-main",
        Some("en"),
        "authorizationprobe OR page_id:denied",
    )
    .await?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].page_id, reader_page_id);
    assert_eq!(hits[0].description, "Reader summary");
    assert_eq!(hits[0].icon.as_deref(), Some("book"));
    let after_last_used: time::PrimitiveDateTime =
        sqlx::query_scalar("SELECT \"lastUsedAt\" FROM reader_session WHERE \"readerId\"=$1")
            .bind(&reader_id)
            .fetch_one(pool)
            .await?;
    assert_eq!(
        before_last_used, after_last_used,
        "admission must not mutate legacy reader activity"
    );

    let reader_fixture = ReaderAdmissionFixture {
        project: readers,
        headers: reader_headers,
        audience_id,
        reader_id,
        reader_token,
        generation,
    };
    assert_reader_grant_transitions(pool, suffix, &reader_fixture).await?;
    fs::remove_dir_all(root)?;
    Ok(())
}

async fn assert_reader_grant_transitions(
    pool: &PgPool,
    suffix: &str,
    fixture: &ReaderAdmissionFixture,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        r#"INSERT INTO audience_grant (id,"projectId","audienceId","pageId","scopeKey","createdAt")
           VALUES ($1,$2,$3,NULL,'SITE',NOW())"#,
    )
    .bind(format!("site-grant-{suffix}"))
    .bind(fixture.project.project_id.as_str())
    .bind(&fixture.audience_id)
    .execute(pool)
    .await?;
    let all_pages =
        admit_published_search(pool, &fixture.project.project_id, &fixture.headers, None)
            .await?
            .ok_or("site-granted reader was not admitted")?;
    assert_eq!(all_pages.kind(), PublishedViewerKind::Reader);
    assert_eq!(all_pages.allowed_page_ids(), None);
    for (version, language, expected) in [
        ("branch-main", "en-GB", "regional"),
        ("branch-main", "fr", "locale"),
        ("branch-v2", "en", "other-version"),
    ] {
        let hits = reader_query(
            &fixture.generation,
            &all_pages,
            version,
            Some(language),
            "*",
        )
        .await?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page_id, expected);
    }

    sqlx::query("DELETE FROM audience_grant WHERE \"audienceId\"=$1")
        .bind(&fixture.audience_id)
        .execute(pool)
        .await?;
    let zero_grant =
        admit_published_search(pool, &fixture.project.project_id, &fixture.headers, None)
            .await?
            .ok_or("active zero-grant reader was not admitted")?;
    assert_eq!(zero_grant.kind(), PublishedViewerKind::Reader);
    assert_eq!(zero_grant.allowed_page_ids(), Some(&BTreeSet::new()));
    assert_eq!(
        reader_query(&fixture.generation, &zero_grant, "branch-main", None, "*")
            .await?
            .len(),
        0
    );

    assert_reader_revocation_and_project_binding(pool, suffix, fixture).await?;
    Ok(())
}

async fn assert_reader_revocation_and_project_binding(
    pool: &PgPool,
    suffix: &str,
    fixture: &ReaderAdmissionFixture,
) -> Result<(), Box<dyn std::error::Error>> {
    let owner = actor("test-owner", "test-owner-session")?;
    // A reader token remains bound to the project-specific cookie name and SQL
    // scope: moving the same live raw value to a different project cannot admit it.
    let unrelated = fixture_project(pool, suffix, "unrelated", "READERS").await?;
    let mut cross_project_headers = HeaderMap::new();
    cross_project_headers.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!(
            "nibleaf_reader_{}={}",
            unrelated.project_id.as_str(),
            fixture.reader_token
        ))?,
    );
    assert!(
        admit_published_search(pool, &unrelated.project_id, &cross_project_headers, None)
            .await?
            .is_none()
    );

    sqlx::query("UPDATE reader_session SET \"revokedAt\"=NOW() WHERE \"readerId\"=$1")
        .bind(&fixture.reader_id)
        .execute(pool)
        .await?;
    assert!(
        admit_published_search(pool, &fixture.project.project_id, &fixture.headers, None)
            .await?
            .is_none()
    );
    add_member(
        pool,
        &fixture.project.organization_id,
        "test-owner",
        &format!("reader-member-{suffix}"),
    )
    .await?;
    let override_admission = admit_published_search(
        pool,
        &fixture.project.project_id,
        &fixture.headers,
        Some(&owner),
    )
    .await?
    .ok_or("workspace override was not admitted")?;
    assert_eq!(override_admission.kind(), PublishedViewerKind::Workspace);
    assert_eq!(override_admission.allowed_page_ids(), None);
    assert_eq!(
        reader_query(
            &fixture.generation,
            &override_admission,
            "branch-v2",
            Some("en"),
            "*"
        )
        .await?[0]
            .page_id,
        "other-version"
    );

    // Even a valid full-site admission cannot be accidentally reused with an
    // unrelated project's generation by an internal route composition bug.
    let other = fixture_project(pool, suffix, "other-public", "PUBLIC").await?;
    let other_admission = admit_published_search(pool, &other.project_id, &HeaderMap::new(), None)
        .await?
        .ok_or("other public site not admitted")?;
    assert!(
        reader_query(
            &fixture.generation,
            &other_admission,
            "branch-main",
            None,
            "*"
        )
        .await
        .is_err()
    );
    Ok(())
}

struct FixtureProject {
    project_id: ProjectId,
    organization_id: String,
}

struct ReaderAdmissionFixture {
    project: FixtureProject,
    headers: HeaderMap,
    audience_id: String,
    reader_id: String,
    reader_token: String,
    generation: ProjectIndexGeneration,
}

async fn build_reader_generation(
    root: &Path,
    project_id: &ProjectId,
    allowed_page_id: &str,
) -> Result<ProjectIndexGeneration, Box<dyn std::error::Error>> {
    let root = root.to_path_buf();
    let project_id = project_id.as_str().to_owned();
    let allowed_page_id = allowed_page_id.to_owned();
    Ok(tokio::task::spawn_blocking(move || {
        let mut documents = Vec::new();
        for (id, language, version_id) in [
            (allowed_page_id.as_str(), "en", "branch-main"),
            ("denied", "en", "branch-main"),
            ("regional", "en-GB", "branch-main"),
            ("locale", "fr", "branch-main"),
            ("other-version", "en", "branch-v2"),
        ] {
            documents.push(SearchDocument {
                page_id: id.to_owned(),
                language: language.to_owned(),
                version_id: version_id.to_owned(),
                version_slug: if version_id == "branch-main" {
                    "main"
                } else {
                    "v2"
                }
                .to_owned(),
                path: "guide".to_owned(),
                title: if id == allowed_page_id {
                    "Unrelated title"
                } else {
                    "authorizationprobe"
                }
                .to_owned(),
                description: "Reader summary".to_owned(),
                icon: Some("book".to_owned()),
                headings: String::new(),
                content: "Read **authorizationprobe** guide".to_owned(),
            });
        }
        build_generation(
            &root,
            &project_id,
            "reader-scope-fixture",
            Uuid::now_v7(),
            &documents,
        )
    })
    .await??)
}

async fn reader_query(
    generation: &ProjectIndexGeneration,
    admission: &PublishedSearchAdmission,
    version: &str,
    language: Option<&str>,
    query: &str,
) -> Result<Vec<SearchHit>, Box<dyn std::error::Error>> {
    let generation = generation.clone();
    let admission = admission.clone();
    let version = version.to_owned();
    let language = language.map(ToOwned::to_owned);
    let query = query.to_owned();
    Ok(tokio::task::spawn_blocking(move || {
        search_admitted_generation(
            &generation,
            &admission,
            &query,
            &version,
            language.as_deref(),
            1,
        )
    })
    .await??)
}

async fn fixture_project(
    pool: &PgPool,
    suffix: &str,
    purpose: &str,
    access_mode: &str,
) -> Result<FixtureProject, Box<dyn std::error::Error>> {
    let organization_id = format!("reader-admission-org-{purpose}-{suffix}");
    let project_id = format!("reader-admission-project-{purpose}-{suffix}");
    sqlx::query("INSERT INTO organization (id,name,slug) VALUES ($1,$2,$3)")
        .bind(&organization_id)
        .bind(format!("Reader admission {purpose}"))
        .bind(format!("reader-admission-{purpose}-{suffix}"))
        .execute(pool)
        .await?;
    sqlx::query(
        r#"INSERT INTO project (id,"organizationId",name,slug,config,"accessMode","createdAt","updatedAt")
           VALUES ($1,$2,$3,$4,'{}'::jsonb,$5::"ProjectAccessMode",NOW(),NOW())"#,
    )
    .bind(&project_id)
    .bind(&organization_id)
    .bind(format!("Reader admission {purpose}"))
    .bind(format!("reader-admission-{purpose}-{suffix}"))
    .bind(access_mode)
    .execute(pool)
    .await?;
    Ok(FixtureProject {
        project_id: ProjectId::try_from(project_id)?,
        organization_id,
    })
}

async fn add_member(
    pool: &PgPool,
    organization_id: &str,
    user_id: &str,
    member_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO member (id,"organizationId","userId",role,"createdAt")
           VALUES ($1,$2,$3,'member',NOW())"#,
    )
    .bind(member_id)
    .bind(organization_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn create_page(
    pool: &PgPool,
    project: &FixtureProject,
    page_id: &str,
) -> Result<(), sqlx::Error> {
    let language_id = format!("reader-language-{}", project.project_id.as_str());
    let branch_id = format!("reader-branch-{}", project.project_id.as_str());
    sqlx::query(
        r#"INSERT INTO language (id,"projectId",code,label,direction,"isDefault",enabled,position,"createdAt","updatedAt")
           VALUES ($1,$2,'en','English','LTR'::"TextDirection",TRUE,TRUE,0,NOW(),NOW())"#,
    )
    .bind(&language_id)
    .bind(project.project_id.as_str())
    .execute(pool)
    .await?;
    sqlx::query(
        r#"INSERT INTO branch (id,"projectId",name,"isDefault","createdAt","updatedAt")
           VALUES ($1,$2,'main',TRUE,NOW(),NOW())"#,
    )
    .bind(&branch_id)
    .bind(project.project_id.as_str())
    .execute(pool)
    .await?;
    sqlx::query(
        r#"INSERT INTO page (id,"projectId","languageId","branchId",kind,title,slug,path,content,position,hidden,"createdAt","updatedAt",revision)
           VALUES ($1,$2,$3,$4,'PAGE'::"PageKind",'Reader page','reader-page','reader-page','Reader content',0,FALSE,NOW(),NOW(),0)"#,
    )
    .bind(page_id)
    .bind(project.project_id.as_str())
    .bind(&language_id)
    .bind(&branch_id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn create_reader_scope(
    pool: &PgPool,
    project_id: &ProjectId,
    reader_id: &str,
    audience_id: &str,
    page_id: &str,
    token: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO reader (id,"projectId",email,status,"createdAt","updatedAt")
           VALUES ($1,$2,'reader@example.test','ACTIVE',NOW(),NOW())"#,
    )
    .bind(reader_id)
    .bind(project_id.as_str())
    .execute(pool)
    .await?;
    sqlx::query(
        r#"INSERT INTO audience (id,"projectId",name,"createdAt","updatedAt")
           VALUES ($1,$2,'Limited',NOW(),NOW())"#,
    )
    .bind(audience_id)
    .bind(project_id.as_str())
    .execute(pool)
    .await?;
    sqlx::query(
        r#"INSERT INTO reader_audience ("readerId","audienceId","createdAt") VALUES ($1,$2,NOW())"#,
    )
    .bind(reader_id)
    .bind(audience_id)
    .execute(pool)
    .await?;
    sqlx::query(
        r#"INSERT INTO audience_grant (id,"projectId","audienceId","pageId","scopeKey","createdAt")
           VALUES ($1,$2,$3,$4,$5,NOW())"#,
    )
    .bind(format!("page-grant-{reader_id}"))
    .bind(project_id.as_str())
    .bind(audience_id)
    .bind(page_id)
    .bind(format!("PAGE:{page_id}"))
    .execute(pool)
    .await?;
    sqlx::query(
        r#"INSERT INTO reader_session (id,"projectId","readerId","tokenHash",source,"expiresAt","lastUsedAt","createdAt")
           VALUES ($1,$2,$3,$4,'INVITATION',NOW() + interval '1 hour',NOW() - interval '10 minutes',NOW())"#,
    )
    .bind(format!("reader-session-{reader_id}"))
    .bind(project_id.as_str())
    .bind(reader_id)
    .bind(hex::encode(Sha256::digest(token.as_bytes())))
    .execute(pool)
    .await?;
    Ok(())
}

fn actor(user_id: &str, session_id: &str) -> Result<Actor, nibleaf_core::AppError> {
    Ok(Actor {
        user_id: UserId::try_from(user_id.to_owned())?,
        session_id: session_id.to_owned(),
        impersonated_by: None,
    })
}
