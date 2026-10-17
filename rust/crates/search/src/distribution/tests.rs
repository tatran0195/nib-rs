use super::*;
use std::{env, fs, sync::Arc, time::Duration};
use uuid::Uuid;
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn object_keys_and_debug_do_not_accept_cross_scope_or_expose_credentials() -> TestResult {
    let id = Uuid::now_v7();
    let mut reference = ArtifactReference {
        generation_id: id,
        project_id: "project".to_owned(),
        tenant_id: "tenant".to_owned(),
        deployment_id: "deployment".to_owned(),
        snapshot_sha256: "a".repeat(64),
        artifact_sha256: "b".repeat(64),
        artifact_bytes: 1,
        documents_sha256: "c".repeat(64),
        document_count: 0,
        object_key: object_key("project", id, &"b".repeat(64)),
    };
    reference.validate()?;
    reference.project_id = "other-project".to_owned();
    assert!(reference.validate().is_err());
    let config = config::S3Config {
        endpoint: "https://private-endpoint.example".to_owned(),
        region: "region".to_owned(),
        bucket: "bucket".to_owned(),
        access_key: "private-access-sentinel".to_owned(),
        secret_key: "private-secret-sentinel".to_owned(),
        allow_http: false,
        timeout: Duration::from_secs(1),
        max_archive_bytes: 1,
    };
    let debug = format!("{config:?}");
    assert!(!debug.contains("private-"));
    assert!(debug.contains("[redacted]"));
    Ok(())
}
#[tokio::test]
async fn atomic_existing_content_is_verified_and_never_overwritten() -> TestResult {
    use object_store::{ObjectStoreExt, path::Path};
    use sha2::{Digest, Sha256};
    let root = env::temp_dir().join(format!("native-store-test-{}", Uuid::now_v7()));
    fs::create_dir(&root)?;
    let bytes = b"test artifact bytes";
    let file = root.join("artifact");
    fs::write(&file, bytes)?;
    let provider = Arc::new(object_store::memory::InMemory::new());
    let store = ArtifactStore::from_provider(provider.clone(), 1024)?;
    let receipt = crate::artifacts::ArtifactReceipt {
        format_version: 1,
        project_id: "project".to_owned(),
        deployment_id: "deployment".to_owned(),
        generation_id: Uuid::now_v7(),
        artifact_sha256: hex::encode(Sha256::digest(bytes)),
        artifact_bytes: bytes.len() as u64,
        documents_sha256: "0".repeat(64),
        document_count: 0,
    };
    store.probe().await?;
    store.put_verified(&file, &receipt).await?;
    store.put_verified(&file, &receipt).await?;
    let key = Path::from(object_key(
        &receipt.project_id,
        receipt.generation_id,
        &receipt.artifact_sha256,
    ));
    provider.put(&key, "bad".into()).await?;
    assert!(store.put_verified(&file, &receipt).await.is_err());
    assert_eq!(provider.get(&key).await?.bytes().await?.as_ref(), b"bad");
    fs::remove_dir_all(root)?;
    Ok(())
}
