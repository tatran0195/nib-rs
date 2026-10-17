#![forbid(unsafe_code)]
//! Read-only verification / new-root rebuild. Never overwrites a serving path.
use nibleaf_search::{
    SearchQueryConfig, build_generation, open_generation, registry::verified_snapshot_documents,
    validate_generation,
};
use serde_json::{Value, json};
use sqlx::{FromRow, postgres::PgPoolOptions};
use std::{env, path::PathBuf};
use uuid::Uuid;

type OpResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(FromRow)]
struct Selected {
    generation_id: Uuid,
    snapshot_sha256: String,
    documents_sha256: Option<String>,
    document_count: Option<i32>,
    snapshot: Option<Value>,
}
#[tokio::main]
async fn main() {
    if run().await.is_err() {
        eprintln!(
            "Search operator command failed; no serving data was modified. Verify arguments, permissions, compatible snapshot/registry/runtime and resource ceilings."
        );
        std::process::exit(2);
    }
}
async fn run() -> OpResult<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    match args.as_slice() {
        [mode, ..] if matches!(mode.as_str(), "verify" | "export-artifact" | "import-artifact") => {
            tokio::task::spawn_blocking(move || filesystem_command(&args)).await??;
        }
        [mode, root, project, deployment] if mode == "rebuild-new-root" || mode == "verify-selected" => {
            process_selected(PathBuf::from(root), project, deployment, mode == "rebuild-new-root").await?;
        }
        _ => return Err("usage: search-ops verify ROOT PROJECT DEPLOYMENT UUID | verify-selected ROOT PROJECT DEPLOYMENT | rebuild-new-root NEW_ROOT PROJECT DEPLOYMENT".into()),
    }
    Ok(())
}
fn filesystem_command(args: &[String]) -> OpResult<()> {
    match args {
        [mode, root, project, deployment, generation] if mode == "verify" => {
            let generation = Uuid::parse_str(generation)?;
            let index = open_generation(&PathBuf::from(root), project, deployment, generation)?;
            let manifest = validate_generation(&index)?;
            println!(
                "{}",
                json!({"status":"verified","project_id":index.project_id(),"deployment_id":index.deployment_id(),
                "generation_id":manifest.generation_id,"documents":manifest.document_count,"documents_sha256":manifest.documents_sha256,"runtime":manifest.runtime})
            );
        }
        [mode, root, project, deployment, generation, output] if mode == "export-artifact" => {
            let generation = open_generation(
                &PathBuf::from(root),
                project,
                deployment,
                Uuid::parse_str(generation)?,
            )?;
            let receipt = nibleaf_search::artifacts::export_artifact(
                &generation,
                &PathBuf::from(output),
                nibleaf_search::artifacts::ArtifactLimits::default(),
            )?;
            println!("{}", serde_json::to_string(&receipt)?);
        }
        [mode, archive, root, project, deployment, generation, digest]
            if mode == "import-artifact" =>
        {
            let expected = nibleaf_search::artifacts::ArtifactExpectation {
                project_id: project.clone(),
                deployment_id: deployment.clone(),
                generation_id: Uuid::parse_str(generation)?,
                artifact_sha256: digest.clone(),
            };
            let receipt = nibleaf_search::artifacts::import_artifact(
                &PathBuf::from(archive),
                &PathBuf::from(root),
                &expected,
                nibleaf_search::artifacts::ArtifactLimits::default(),
            )?;
            println!("{}", serde_json::to_string(&receipt)?);
        }
        _ => return Err("invalid filesystem operator command".into()),
    }
    Ok(())
}

async fn selected_source(
    project: &str,
    deployment: &str,
    config: SearchQueryConfig,
) -> OpResult<Selected> {
    let database = env::var("NIBLEAF_DATABASE_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database)
        .await?;
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT set_config('statement_timeout',$1,true)")
        .bind(format!("{}ms", config.timeout.as_millis()))
        .execute(&mut *tx)
        .await?;
    let selected = sqlx::query_as::<_, Selected>(r#"
      WITH latest AS MATERIALIZED (
        SELECT id,"projectId",snapshot FROM deployment WHERE "projectId"=$1 AND status='READY'
        ORDER BY version DESC,id DESC LIMIT 1
      )
      SELECT generation.generation_id,generation.snapshot_sha256,generation.documents_sha256,generation.document_count,
             CASE WHEN octet_length(latest.snapshot::text)<=$3 THEN latest.snapshot ELSE NULL END AS snapshot
      FROM latest INNER JOIN project ON project.id=latest."projectId"
      INNER JOIN rust_search_generation AS generation ON generation.project_id=latest."projectId"
        AND generation.tenant_id=project."organizationId" AND generation.deployment_id=latest.id
      WHERE latest.id=$2 AND generation.state IN ('ready','superseded')"#)
        .bind(project).bind(deployment).bind(i64::try_from(config.max_snapshot_bytes)?).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    pool.close().await;
    Ok(selected)
}
async fn process_selected(
    root: PathBuf,
    project: &str,
    deployment: &str,
    rebuild: bool,
) -> OpResult<()> {
    let config = SearchQueryConfig::from_env()?;
    let selected = selected_source(project, deployment, config).await?;
    let snapshot = selected.snapshot.ok_or("snapshot is oversized")?;
    let project = project.to_owned();
    let deployment = deployment.to_owned();
    let manifest = tokio::task::spawn_blocking(move || -> OpResult<_> {
        let documents =
            verified_snapshot_documents(&snapshot, &project, &selected.snapshot_sha256, config)?;
        if documents.len() > config.max_documents {
            return Err("document limit".into());
        }
        // Atomic creation rejects existing directories AND symlinks, including
        // a caller accidentally supplying the live root. No deletion/rename of
        // any pre-existing root, generation or database state occurs.
        let generation = if rebuild {
            let mut directory = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                directory.mode(0o700);
            }
            directory.create(&root)?;
            build_generation(
                &root,
                &project,
                &deployment,
                selected.generation_id,
                &documents,
            )?
        } else {
            let expected = nibleaf_search::documents_digest(&documents)?;
            if selected.documents_sha256.as_deref() != Some(expected.documents_sha256.as_str())
                || selected.document_count.and_then(|n| u64::try_from(n).ok())
                    != Some(expected.document_count)
            {
                return Err("source digest mismatch".into());
            }
            open_generation(&root, &project, &deployment, selected.generation_id)?
        };
        let manifest = validate_generation(&generation)?;
        if selected
            .document_count
            .and_then(|count| u64::try_from(count).ok())
            != Some(manifest.document_count)
            || selected.documents_sha256.as_deref() != Some(manifest.documents_sha256.as_str())
        {
            return Err("rebuilt source identity differs from registry".into());
        }
        Ok(manifest)
    })
    .await??;
    println!(
        "{}",
        json!({"status":if rebuild {"rebuilt_offline_verified"} else {"selected_verified"},"generation_id":manifest.generation_id,"documents":manifest.document_count,
        "documents_sha256":manifest.documents_sha256,"runtime":manifest.runtime,"serving_pointer_changed":false})
    );
    Ok(())
}
