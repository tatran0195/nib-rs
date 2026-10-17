use super::*;
use crate::{SearchDocument, build_generation, open_generation, search_generation};
use std::env;
type TestResult = Result<(), Box<dyn std::error::Error>>;
fn fixture() -> Result<(std::path::PathBuf, ProjectIndexGeneration), SearchError> {
    let root = env::temp_dir().join(format!("nibleaf-artifact-test-{}", Uuid::now_v7()));
    fs::create_dir(&root)?;
    let doc = SearchDocument {
        page_id: "page".to_owned(),
        language: "en".to_owned(),
        version_id: "v1".to_owned(),
        version_slug: "main".to_owned(),
        path: "guide".to_owned(),
        title: "Portable scopeword".to_owned(),
        description: "summary".to_owned(),
        icon: None,
        headings: String::new(),
        content: "scopeword immutable source".to_owned(),
    };
    let generation = build_generation(
        &root.join("source"),
        "artifact-project",
        "artifact-publication",
        Uuid::now_v7(),
        &[doc],
    )?;
    Ok((root, generation))
}
fn expectation(receipt: &ArtifactReceipt) -> ArtifactExpectation {
    ArtifactExpectation {
        project_id: receipt.project_id.clone(),
        deployment_id: receipt.deployment_id.clone(),
        generation_id: receipt.generation_id,
        artifact_sha256: receipt.artifact_sha256.clone(),
    }
}
#[test]
fn portable_generation_uses_an_unshared_private_root_and_preserves_exact_source() -> TestResult {
    let (root, generation) = fixture()?;
    let artifact = root.join("generation.tar.zst");
    let limits = ArtifactLimits::default();
    let receipt = export_artifact(&generation, &artifact, limits)?;
    let imported = import_artifact(
        &artifact,
        &root.join("replica"),
        &expectation(&receipt),
        limits,
    )?;
    assert_eq!(imported.documents_sha256, receipt.documents_sha256);
    assert_eq!(imported.artifact_sha256, receipt.artifact_sha256);
    let replica = open_generation(
        &root.join("replica"),
        generation.project_id(),
        generation.deployment_id(),
        generation.generation_id(),
    )?;
    assert_eq!(
        search_generation(&generation, "scopeword", 12)?,
        search_generation(&replica, "scopeword", 12)?
    );
    assert!(
        import_artifact(
            &artifact,
            &root.join("replica"),
            &expectation(&receipt),
            limits
        )
        .is_err()
    );
    assert!(export_artifact(&generation, &artifact, limits).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(root.join("replica"))?.permissions().mode() & 0o077,
            0
        );
        assert_eq!(fs::metadata(&artifact)?.permissions().mode() & 0o077, 0);
    }
    fs::remove_dir_all(root)?;
    Ok(())
}
#[test]
fn scope_digest_and_size_refusals_do_not_create_a_replica_root() -> TestResult {
    let (root, generation) = fixture()?;
    let artifact = root.join("generation.tar.zst");
    let limits = ArtifactLimits::default();
    let receipt = export_artifact(&generation, &artifact, limits)?;
    let mut wrong = expectation(&receipt);
    wrong.project_id = "other-project".to_owned();
    assert!(import_artifact(&artifact, &root.join("wrong-project"), &wrong, limits).is_err());
    assert!(!root.join("wrong-project").exists());
    let mut wrong = expectation(&receipt);
    wrong.artifact_sha256 = "0".repeat(64);
    assert!(import_artifact(&artifact, &root.join("wrong-digest"), &wrong, limits).is_err());
    for (tag, limited) in [
        (
            "compressed",
            ArtifactLimits {
                max_compressed_bytes: 1,
                ..limits
            },
        ),
        (
            "unpacked",
            ArtifactLimits {
                max_unpacked_bytes: 1,
                ..limits
            },
        ),
        (
            "files",
            ArtifactLimits {
                max_files: 1,
                ..limits
            },
        ),
        (
            "manifest",
            ArtifactLimits {
                max_manifest_bytes: 1,
                ..limits
            },
        ),
    ] {
        assert!(
            import_artifact(&artifact, &root.join(tag), &expectation(&receipt), limited).is_err()
        );
        assert!(!root.join(tag).exists());
    }
    assert!(
        export_artifact(
            &generation,
            &generation.directory().join("bad-export"),
            limits
        )
        .is_err()
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

fn rewrite(
    root: &Path,
    original: &Path,
    name: &str,
    modify: impl FnOnce(&mut Vec<(tar::Header, Vec<u8>)>) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(std::path::PathBuf, String), Box<dyn std::error::Error>> {
    let mut archive = tar::Archive::new(zstd::stream::read::Decoder::new(File::open(original)?)?);
    let mut entries = Vec::new();
    for entry in archive.entries()?.raw(true) {
        let mut entry = entry?;
        let mut content = Vec::new();
        entry.read_to_end(&mut content)?;
        entries.push((entry.header().clone(), content));
    }
    modify(&mut entries)?;
    let output = root.join(name);
    let encoder = zstd::stream::write::Encoder::new(File::create(&output)?, 3)?;
    let mut builder = tar::Builder::new(encoder);
    for (mut header, content) in entries {
        header.set_size(content.len() as u64);
        header.set_cksum();
        builder.append(&header, content.as_slice())?;
    }
    builder.into_inner()?.finish()?;
    let digest = hash_reader(&mut File::open(&output)?, u64::MAX)?.sha256;
    Ok((output, digest))
}
#[test]
fn malformed_special_duplicate_missing_and_corrupt_payloads_never_import_successfully() -> TestResult
{
    let (root, generation) = fixture()?;
    let artifact = root.join("original.zst");
    let limits = ArtifactLimits::default();
    let receipt = export_artifact(&generation, &artifact, limits)?;
    for tag in [
        "traversal",
        "symlink",
        "duplicate",
        "missing",
        "payload",
        "format",
        "duplicate-json",
    ] {
        let (bad, sha256) = rewrite(&root, &artifact, tag, |entries| {
            match tag {
                "traversal" => {
                    let header = &mut entries[1].0;
                    let raw = header.as_mut_bytes();
                    raw[..100].fill(0);
                    raw[..6].copy_from_slice(b"../bad");
                }
                "symlink" => {
                    entries[1].0.set_entry_type(tar::EntryType::Symlink);
                    entries[1].0.set_link_name("/tmp/forbidden")?;
                }
                "duplicate" => entries.push(entries[1].clone()),
                "missing" => {
                    entries.pop();
                }
                "payload" => {
                    assert_ne!(entries[1].1.len(), 0);
                    entries[1].1[0] ^= 1;
                }
                "duplicate-json" => {
                    let value: serde_json::Value = serde_json::from_slice(&entries[0].1)?;
                    let raw = String::from_utf8(entries[0].1.clone())?;
                    let extra = serde_json::to_string(&value["files"]["manifest.json"])?;
                    entries[0].1 = raw
                        .replace(
                            "\"files\":{",
                            &format!("\"files\":{{\"manifest.json\":{extra},"),
                        )
                        .into_bytes();
                }
                "format" => {
                    let mut value: serde_json::Value = serde_json::from_slice(&entries[0].1)?;
                    value["format_version"] = serde_json::json!(99);
                    entries[0].1 = serde_json::to_vec(&value)?;
                }
                _ => return Err("unknown mutation".into()),
            }
            Ok(())
        })?;
        let mut expected = expectation(&receipt);
        expected.artifact_sha256 = sha256;
        assert!(
            import_artifact(
                &bad,
                &root.join(format!("replica-{tag}")),
                &expected,
                limits
            )
            .is_err(),
            "bad envelope accepted"
        );
    }
    fs::remove_dir_all(root)?;
    Ok(())
}
#[cfg(unix)]
#[test]
fn source_symlinks_are_not_followed_into_an_artifact() -> TestResult {
    use std::os::unix::fs::symlink;
    let (root, generation) = fixture()?;
    fs::write(root.join("outside"), "not an index file")?;
    symlink(root.join("outside"), generation.directory().join("link"))?;
    assert!(
        export_artifact(
            &generation,
            &root.join("bad.zst"),
            ArtifactLimits::default()
        )
        .is_err()
    );
    assert!(!root.join("bad.zst").exists());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn runtime_import_obeys_deadline_and_source_budgets_before_a_success_receipt() -> TestResult {
    use crate::execution::budget::QueryBudget;
    use std::time::Duration;
    use tokio::time::Instant;
    let (root, generation) = fixture()?;
    let file = root.join("runtime.zst");
    let limits = ArtifactLimits::default();
    let receipt = export_artifact(&generation, &file, limits)?;
    let expected = expectation(&receipt);
    let now = Instant::now();
    let budget = QueryBudget {
        config: crate::SearchQueryConfig::default(),
        started: now,
        deadline: now - Duration::from_secs(1),
        cancellation: tokio_util::sync::CancellationToken::new(),
    };
    assert!(matches!(
        import_artifact_runtime(&file, &root.join("expired"), &expected, limits, &budget),
        Err(SearchError::Deadline)
    ));
    assert!(!root.join("expired").exists());
    let budget = QueryBudget {
        config: crate::SearchQueryConfig {
            max_source_bytes: 1,
            ..crate::SearchQueryConfig::default()
        },
        started: now,
        deadline: now + Duration::from_secs(5),
        cancellation: tokio_util::sync::CancellationToken::new(),
    };
    assert!(matches!(
        import_artifact_runtime(
            &file,
            &root.join("source-limit"),
            &expected,
            limits,
            &budget
        ),
        Err(SearchError::ResourceLimit(_))
    ));
    fs::remove_dir_all(root)?;
    Ok(())
}
