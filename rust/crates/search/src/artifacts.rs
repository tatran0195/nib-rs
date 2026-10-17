//! Portable immutable generation envelopes for unshared replica roots.
//!
//! These are offline/operator distribution primitives, not an object-store
//! publisher, replica acknowledgement, GC/erasure proof or production-ready
//! multi-host controller. Import never overwrites an existing root. A serving
//! replica must still verify the selected PostgreSQL snapshot/registry binding.
use crate::{
    ProjectIndexGeneration, SearchError, generation_directory, validate_generation,
    validate_identifier,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Seek, Write},
    path::Path,
};
use uuid::Uuid;

const DESCRIPTOR: &str = "artifact.json";
const FORMAT: u16 = 1;
const MAX_DESCRIPTOR_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct ArtifactLimits {
    pub max_compressed_bytes: u64,
    pub max_unpacked_bytes: u64,
    pub max_manifest_bytes: u64,
    pub max_files: usize,
}
impl Default for ArtifactLimits {
    fn default() -> Self {
        Self {
            max_compressed_bytes: 512 * 1024 * 1024,
            max_unpacked_bytes: 1024 * 1024 * 1024,
            max_manifest_bytes: 16 * 1024 * 1024,
            max_files: 4096,
        }
    }
}
impl ArtifactLimits {
    fn check(self) -> Result<(), SearchError> {
        if self.max_compressed_bytes == 0
            || self.max_unpacked_bytes == 0
            || self.max_manifest_bytes == 0
            || self.max_files == 0
        {
            return Err(SearchError::InvalidInput(
                "artifact limits must be positive",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ArtifactExpectation {
    pub project_id: String,
    pub deployment_id: String,
    pub generation_id: Uuid,
    /// Trusted control-plane/producer digest, not browser authority.
    pub artifact_sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactReceipt {
    pub format_version: u16,
    pub project_id: String,
    pub deployment_id: String,
    pub generation_id: Uuid,
    pub artifact_sha256: String,
    pub artifact_bytes: u64,
    pub documents_sha256: String,
    pub document_count: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileIdentity {
    bytes: u64,
    sha256: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    format_version: u16,
    project_id: String,
    deployment_id: String,
    generation_id: Uuid,
    documents_sha256: String,
    document_count: u64,
    #[serde(deserialize_with = "file_map")]
    files: BTreeMap<String, FileIdentity>,
}

fn file_map<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, FileIdentity>, D::Error> {
    struct UniqueFiles;
    impl<'de> serde::de::Visitor<'de> for UniqueFiles {
        type Value = BTreeMap<String, FileIdentity>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a unique artifact file map")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut files = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, FileIdentity>()? {
                if files.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate artifact file"));
                }
            }
            Ok(files)
        }
    }
    deserializer.deserialize_map(UniqueFiles)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 96
        && !matches!(name, "." | ".." | DESCRIPTOR)
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn invalid() -> SearchError {
    SearchError::Integrity("artifact envelope or source binding is invalid")
}

fn hash_reader(reader: &mut impl Read, maximum: u64) -> Result<FileIdentity, SearchError> {
    hash_reader_with_budget(reader, maximum, None)
}
fn hash_reader_with_budget(
    reader: &mut impl Read,
    maximum: u64,
    budget: Option<&crate::execution::budget::QueryBudget>,
) -> Result<FileIdentity, SearchError> {
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        crate::execution::budget::checkpoint(budget)?;
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(u64::try_from(count).map_err(|_| invalid())?)
            .ok_or_else(invalid)?;
        if bytes > maximum {
            return Err(SearchError::ResourceLimit("artifact bytes"));
        }
        digest.update(&buffer[..count]);
    }
    Ok(FileIdentity {
        bytes,
        sha256: hex::encode(digest.finalize()),
    })
}
fn source_files(
    generation: &ProjectIndexGeneration,
    limits: ArtifactLimits,
) -> Result<BTreeMap<String, FileIdentity>, SearchError> {
    let mut files = BTreeMap::new();
    let mut total = 0_u64;
    for entry in fs::read_dir(generation.directory())? {
        let entry = entry?;
        if files.len() >= limits.max_files {
            return Err(SearchError::ResourceLimit("artifact file count"));
        }
        let name = entry.file_name().into_string().map_err(|_| invalid())?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if !valid_name(&name) || !metadata.file_type().is_file() {
            return Err(invalid());
        }
        let identity = hash_reader(
            &mut File::open(entry.path())?,
            limits.max_unpacked_bytes.saturating_sub(total),
        )?;
        total = total.checked_add(identity.bytes).ok_or_else(invalid)?;
        files.insert(name, identity);
    }
    check_files(&files, limits)?;
    Ok(files)
}
fn check_files(
    files: &BTreeMap<String, FileIdentity>,
    limits: ArtifactLimits,
) -> Result<(), SearchError> {
    if files.len() > limits.max_files {
        return Err(SearchError::ResourceLimit("artifact file count"));
    }
    let mut total = 0_u64;
    for (name, file) in files {
        if !valid_name(name) || !valid_digest(&file.sha256) {
            return Err(invalid());
        }
        total = total.checked_add(file.bytes).ok_or_else(invalid)?;
        if total > limits.max_unpacked_bytes {
            return Err(SearchError::ResourceLimit("artifact unpacked bytes"));
        }
    }
    if files
        .get("manifest.json")
        .is_none_or(|file| file.bytes > limits.max_manifest_bytes)
    {
        return Err(invalid());
    }
    Ok(())
}

struct DigestInput {
    file: File,
    digest: Sha256,
    bytes: u64,
}
impl Read for DigestInput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.file.read(buffer)?;
        self.digest.update(&buffer[..count]);
        self.bytes += count as u64;
        Ok(count)
    }
}

struct LimitedOutput {
    file: File,
    digest: Sha256,
    bytes: u64,
    maximum: u64,
}
impl Write for LimitedOutput {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if u64::try_from(buffer.len()).map_or(true, |n| n > self.maximum.saturating_sub(self.bytes))
        {
            return Err(io::Error::other("compressed artifact limit"));
        }
        let count = self.file.write(buffer)?;
        self.digest.update(&buffer[..count]);
        self.bytes += count as u64;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
fn private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}
fn private_root(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}
fn append_entry<W: Write>(
    builder: &mut tar::Builder<W>,
    name: &str,
    size: u64,
    reader: impl Read,
) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(size);
    header.set_mode(0o600);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    builder.append_data(&mut header, name, reader)
}

/// Exports one verified immutable generation into a new private tar+zstd file.
/// No source/serving file or existing output is overwritten. Failure can leave
/// a private partial output without a success receipt; never publish it.
/// # Errors
/// Rejects invalid/corrupt source, special paths/files, limits, existing output
/// and writes inside the source generation; preserves storage/codec failures.
pub fn export_artifact(
    generation: &ProjectIndexGeneration,
    output: &Path,
    limits: ArtifactLimits,
) -> Result<ArtifactReceipt, SearchError> {
    limits.check()?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if parent
        .canonicalize()?
        .starts_with(generation.directory().canonicalize()?)
    {
        return Err(invalid());
    }
    let files = source_files(generation, limits)?;
    let manifest = validate_generation(generation)?;
    let descriptor = Descriptor {
        format_version: FORMAT,
        project_id: generation.project_id().to_owned(),
        deployment_id: generation.deployment_id().to_owned(),
        generation_id: generation.generation_id(),
        documents_sha256: manifest.documents_sha256,
        document_count: manifest.document_count,
        files,
    };
    let bytes = serde_json::to_vec(&descriptor)?;
    if bytes.len() as u64 > MAX_DESCRIPTOR_BYTES {
        return Err(SearchError::ResourceLimit("artifact descriptor bytes"));
    }
    let writer = LimitedOutput {
        file: private_file(output)?,
        digest: Sha256::new(),
        bytes: 0,
        maximum: limits.max_compressed_bytes,
    };
    let mut encoder = zstd::stream::write::Encoder::new(writer, 3)?;
    encoder.window_log(23)?;
    let mut builder = tar::Builder::new(encoder);
    append_entry(
        &mut builder,
        DESCRIPTOR,
        bytes.len() as u64,
        bytes.as_slice(),
    )?;
    for (name, identity) in &descriptor.files {
        let mut reader = DigestInput {
            file: File::open(generation.directory().join(name))?,
            digest: Sha256::new(),
            bytes: 0,
        };
        append_entry(&mut builder, name, identity.bytes, &mut reader)?;
        let mut extra = [0_u8; 1];
        if reader.bytes != identity.bytes
            || hex::encode(reader.digest.clone().finalize()) != identity.sha256
            || reader.read(&mut extra)? != 0
        {
            return Err(invalid());
        }
    }
    let encoder = builder.into_inner()?;
    let mut writer = encoder.finish()?;
    writer.flush()?;
    writer.file.sync_all()?;
    Ok(receipt(
        &descriptor,
        hex::encode(writer.digest.finalize()),
        writer.bytes,
    ))
}
fn receipt(descriptor: &Descriptor, sha256: String, bytes: u64) -> ArtifactReceipt {
    ArtifactReceipt {
        format_version: FORMAT,
        project_id: descriptor.project_id.clone(),
        deployment_id: descriptor.deployment_id.clone(),
        generation_id: descriptor.generation_id,
        artifact_sha256: sha256,
        artifact_bytes: bytes,
        documents_sha256: descriptor.documents_sha256.clone(),
        document_count: descriptor.document_count,
    }
}

fn read_descriptor<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    expected: &ArtifactExpectation,
    limits: ArtifactLimits,
) -> Result<Descriptor, SearchError> {
    if entry.path_bytes().as_ref() != DESCRIPTOR.as_bytes()
        || !entry.header().entry_type().is_file()
        || entry.size() > MAX_DESCRIPTOR_BYTES
    {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes)?;
    let descriptor: Descriptor = serde_json::from_slice(&bytes)?;
    if descriptor.format_version != FORMAT
        || descriptor.project_id != expected.project_id
        || descriptor.deployment_id != expected.deployment_id
        || descriptor.generation_id != expected.generation_id
        || !valid_digest(&descriptor.documents_sha256)
    {
        return Err(invalid());
    }
    check_files(&descriptor.files, limits)?;
    Ok(descriptor)
}
fn unpack_entry<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    target: &Path,
    descriptor: &Descriptor,
    seen: &mut BTreeSet<String>,
    budget: Option<&crate::execution::budget::QueryBudget>,
) -> Result<(), SearchError> {
    let raw = entry.path_bytes();
    let name = std::str::from_utf8(&raw).map_err(|_| invalid())?;
    if !valid_name(name) || !entry.header().entry_type().is_file() || !seen.insert(name.to_owned())
    {
        return Err(invalid());
    }
    let identity = descriptor.files.get(name).ok_or_else(invalid)?;
    if entry.size() != identity.bytes {
        return Err(invalid());
    }
    let mut output = private_file(&target.join(name))?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        crate::execution::budget::checkpoint(budget)?;
        let n = entry.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > identity.bytes {
            return Err(invalid());
        }
        digest.update(&buffer[..n]);
        output.write_all(&buffer[..n])?;
    }
    output.sync_all()?;
    if size != identity.bytes || hex::encode(digest.finalize()) != identity.sha256 {
        return Err(invalid());
    }
    Ok(())
}

/// Imports only into a brand-new private root, validates the trusted digest,
/// exact project/deployment/generation and every flat regular file, then fully
/// validates native runtime/source/index evidence. This never changes a DB or
/// serving pointer; it is not offline-replica purge acknowledgement.
/// # Errors
/// Rejects scope/digest/format mismatch, traversal/symlink/hardlink/PAX/sparse/
/// duplicate/unknown/missing entries, oversized data/window, and existing roots.
pub fn import_artifact(
    archive: &Path,
    new_root: &Path,
    expected: &ArtifactExpectation,
    limits: ArtifactLimits,
) -> Result<ArtifactReceipt, SearchError> {
    import_artifact_with_size(archive, new_root, expected, limits).map(|(receipt, _)| receipt)
}

pub(crate) fn import_artifact_with_size(
    archive: &Path,
    new_root: &Path,
    expected: &ArtifactExpectation,
    limits: ArtifactLimits,
) -> Result<(ArtifactReceipt, u64), SearchError> {
    import_checked(archive, new_root, expected, limits, None)
}
pub(crate) fn import_artifact_runtime(
    archive: &Path,
    new_root: &Path,
    expected: &ArtifactExpectation,
    limits: ArtifactLimits,
    budget: &crate::execution::budget::QueryBudget,
) -> Result<(ArtifactReceipt, u64), SearchError> {
    import_checked(archive, new_root, expected, limits, Some(budget))
}
fn import_checked(
    archive: &Path,
    new_root: &Path,
    expected: &ArtifactExpectation,
    limits: ArtifactLimits,
    budget: Option<&crate::execution::budget::QueryBudget>,
) -> Result<(ArtifactReceipt, u64), SearchError> {
    limits.check()?;
    validate_identifier(&expected.project_id)?;
    validate_identifier(&expected.deployment_id)?;
    if !valid_digest(&expected.artifact_sha256)
        || !fs::symlink_metadata(archive)?.file_type().is_file()
    {
        return Err(invalid());
    }
    let mut file = File::open(archive)?;
    let compressed = hash_reader_with_budget(&mut file, limits.max_compressed_bytes, budget)?;
    if compressed.sha256 != expected.artifact_sha256 {
        return Err(invalid());
    }
    file.rewind()?;
    let mut decoder = zstd::stream::read::Decoder::new(BufReader::new(file))?;
    decoder.window_log_max(23)?;
    let mut archive = tar::Archive::new(decoder);
    let mut entries = archive.entries()?.raw(true);
    let mut first = entries.next().ok_or_else(invalid)??;
    let descriptor = read_descriptor(&mut first, expected, limits)?;
    drop(first);
    crate::execution::budget::checkpoint(budget)?;
    if budget.is_some_and(|b| descriptor.document_count > b.config.max_documents as u64) {
        return Err(SearchError::ResourceLimit("artifact document count"));
    }
    private_root(new_root)?;
    let target = generation_directory(new_root, &expected.project_id, expected.generation_id);
    fs::create_dir_all(&target)?;
    let mut seen = BTreeSet::new();
    for entry in entries {
        unpack_entry(&mut entry?, &target, &descriptor, &mut seen, budget)?;
    }
    if seen.len() != descriptor.files.len() {
        return Err(invalid());
    }
    let decoder = archive.into_inner();
    let mut tail = Vec::new();
    decoder.take(2049).read_to_end(&mut tail)?;
    if tail.len() > 2048 || tail.iter().any(|byte| *byte != 0) {
        return Err(invalid());
    }
    let (_, manifest) = crate::open_generation_with_budget(
        new_root,
        &expected.project_id,
        &expected.deployment_id,
        expected.generation_id,
        budget,
    )?;
    if manifest.documents_sha256 != descriptor.documents_sha256
        || manifest.document_count != descriptor.document_count
    {
        return Err(invalid());
    }
    let unpacked = descriptor
        .files
        .values()
        .try_fold(0_u64, |total, file| total.checked_add(file.bytes))
        .ok_or_else(invalid)?;
    Ok((
        receipt(&descriptor, compressed.sha256, compressed.bytes),
        unpacked,
    ))
}

#[cfg(test)]
mod tests;
