#![forbid(unsafe_code)]

//! Project-isolated lexical search generations backed by Tantivy and Lindera.
//!
//! This crate intentionally has no embedding model, vector field, global
//! corpus, or cross-project query API. A caller builds one immutable generation
//! from one deployment, then a later durable registry chooses which generation
//! is readable for that project.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tantivy::{
    Index, TantivyDocument,
    collector::TopDocs,
    doc,
    query::{AllQuery, QueryParserError},
    schema::{
        Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value,
    },
};
use thiserror::Error;
use uuid::Uuid;

mod api;
pub mod artifacts;
pub mod distribution;
mod execution;
pub mod public_admission;
mod public_api;
mod publication_scope;
mod published;
mod query;
pub mod registry;
mod runtime;
mod snapshot;
mod text;
mod tokenizer;

pub use api::router;
use execution::budget::{QueryBudget, checkpoint};
pub use execution::{
    SearchQueryConfig, SearchQueryConfigError, SearchQueryDriver, SearchQueryExecutionError,
    SearchQueryService,
};
pub use public_admission::{PublishedSearchAdmission, PublishedViewerKind, admit_published_search};
pub use public_api::{
    PUBLIC_SEARCH_MATCHED_PATH, apply_private_policy as apply_private_search_policy, public_router,
};
pub use publication_scope::PublishedSearchContext;
pub use published::{
    ActorResolution, PreparedPublishedSearch, PublishedActorResolver, PublishedSearchError,
    PublishedSearchRequest, PublishedSearchResult, prepare_published_search,
};
pub use query::{
    search_admitted_generation, search_generation, search_generation_for_language,
    search_generation_for_version,
};
pub use runtime::GenerationRuntime;
use tokenizer::{LinderaTokenizer, TokenizerHealth};

const MANIFEST_FILE_NAME: &str = "manifest.json";
const MANIFEST_FORMAT_VERSION: u16 = 3;
const JAPANESE_TOKENIZER_NAME: &str = "nibleaf_lindera_ipadic";
const CHINESE_TOKENIZER_NAME: &str = "nibleaf_lindera_cc_cedict";
const MAX_QUERY_BYTES: usize = 512;
const MAX_RESULTS: usize = 50;
const MAX_DOCUMENT_TEXT_BYTES: usize = 1_000_000;

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("search work was cancelled")]
    Cancelled,
    #[error("search deadline expired")]
    Deadline,
    #[error("search resource limit exceeded: {0}")]
    ResourceLimit(&'static str),
    #[error("search distribution operation failed: {0}")]
    Distribution(&'static str),
    #[error("search input is invalid: {0}")]
    InvalidInput(&'static str),
    #[error("search generation integrity validation failed: {0}")]
    Integrity(&'static str),
    #[error("search storage failed")]
    Storage(#[from] std::io::Error),
    #[error("search manifest is malformed")]
    Manifest(#[from] serde_json::Error),
    #[error("tantivy operation failed")]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("query syntax is invalid")]
    Query(#[from] QueryParserError),
    #[error("Lindera tokenizer initialization or segmentation failed")]
    Tokenizer(#[source] std::sync::Arc<lindera::error::LinderaError>),
}

/// One page representation from an immutable deployment snapshot. Search
/// displays only stored routing/result fields; it never needs a database-wide
/// post-filter to establish project scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchDocument {
    pub page_id: String,
    /// Exact immutable deployment language code, not merely a tokenizer family.
    pub language: String,
    /// Opaque immutable branch/version identity from the same snapshot.
    pub version_id: String,
    /// Immutable branch/version slug resolved at publication time.
    pub version_slug: String,
    pub path: String,
    pub title: String,
    pub description: String,
    pub icon: Option<String>,
    pub headings: String,
    pub content: String,
}

/// Persisted, versioned evidence that a generation was built from exactly one
/// deployment snapshot. The document checksum map is ordered by opaque page ID
/// and intentionally contains no page content. Its aggregate checksum permits a
/// later registry to bind this generation to an independently calculated
/// deployment-snapshot digest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationManifest {
    pub format_version: u16,
    pub runtime: GenerationRuntime,
    pub generation_id: Uuid,
    pub project_id: String,
    pub deployment_id: String,
    pub document_count: u64,
    pub document_checksums: BTreeMap<String, String>,
    pub documents_sha256: String,
}

/// Compact, deterministic source-document identity used to bind a manifest to
/// a deployment snapshot without putting page content in PostgreSQL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentDigest {
    pub document_count: u64,
    pub documents_sha256: String,
}

/// Identity and filesystem location of one immutable built index generation.
/// The location is private so callers cannot construct a generation pointing at
/// an arbitrary path. Use [`open_generation`] to open an already-built one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectIndexGeneration {
    deployment_id: String,
    generation_id: Uuid,
    project_id: String,
    directory: PathBuf,
}

impl ProjectIndexGeneration {
    #[must_use]
    pub fn deployment_id(&self) -> &str {
        &self.deployment_id
    }

    #[must_use]
    pub fn generation_id(&self) -> Uuid {
        self.generation_id
    }

    #[must_use]
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    /// Returns the manifest path for operational inspection. Callers must not
    /// mutate it; [`validate_generation`] is the gate used before promotion.
    #[must_use]
    pub fn manifest_path(&self) -> PathBuf {
        self.directory.join(MANIFEST_FILE_NAME)
    }

    #[must_use]
    fn directory(&self) -> &Path {
        &self.directory
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub version_id: String,
    pub version_slug: String,
    pub language: String,
    pub page_id: String,
    pub path: String,
    pub score: f32,
    pub title: String,
    pub description: String,
    pub icon: Option<String>,
    pub snippet: String,
}

#[derive(Clone, Copy)]
struct AnalyzedFields {
    title: Field,
    headings: Field,
    description: Field,
    content: Field,
}

#[derive(Clone, Copy)]
struct SearchFields {
    chinese: AnalyzedFields,
    japanese: AnalyzedFields,
    general: AnalyzedFields,
    deployment_id: Field,
    description: Field,
    document_checksum: Field,
    headings: Field,
    icon: Field,
    language: Field,
    page_id: Field,
    path: Field,
    project_id: Field,
    source_content: Field,
    title: Field,
    version_id: Field,
    version_slug: Field,
}

impl SearchFields {
    fn analyzed(self, language: &str) -> AnalyzedFields {
        match language_family(language) {
            LanguageFamily::Japanese => self.japanese,
            LanguageFamily::Chinese => self.chinese,
            LanguageFamily::General => self.general,
        }
    }
}

/// Builds a new project-scoped immutable generation. Tantivy data and its
/// manifest are prepared under an unpublishable staging name, validated, then
/// moved atomically into the generation path. It never writes a `current`
/// pointer or removes another generation; durable promotion belongs to the
/// PostgreSQL registry slice.
///
/// # Errors
/// Returns an error for invalid identifiers/documents, a pre-existing final or
/// staging directory, tokenizer initialization, manifest/integrity validation,
/// filesystem, or Tantivy failure. A failed staging directory is deliberately
/// retained for a later leased worker reclaim/cleanup policy; it is never a
/// readable generation path.
pub fn build_generation(
    index_root: &Path,
    project_id: &str,
    deployment_id: &str,
    generation_id: Uuid,
    documents: &[SearchDocument],
) -> Result<ProjectIndexGeneration, SearchError> {
    validate_identifier(project_id)?;
    validate_identifier(deployment_id)?;
    let manifest = manifest_for(project_id, deployment_id, generation_id, documents)?;

    let directory = generation_directory(index_root, project_id, generation_id);
    if directory.exists() {
        return Err(SearchError::InvalidInput("index generation already exists"));
    }
    let staging_directory = staging_directory(index_root, project_id, generation_id);
    if staging_directory.exists() {
        return Err(SearchError::InvalidInput(
            "index generation staging directory already exists",
        ));
    }
    let parent = directory
        .parent()
        .ok_or(SearchError::Integrity("generation directory has no parent"))?;
    fs::create_dir_all(parent)?;
    fs::create_dir(&staging_directory)?;

    let schema = schema();
    let fields = fields(&schema)?;
    let index = Index::create_in_dir(&staging_directory, schema)?;
    let tokenizers = register_lindera(&index)?;
    write_documents(
        &index,
        fields,
        project_id,
        deployment_id,
        documents,
        &tokenizers,
    )?;

    write_manifest(&staging_directory, &manifest)?;
    let staged_generation = ProjectIndexGeneration {
        deployment_id: deployment_id.to_owned(),
        generation_id,
        project_id: project_id.to_owned(),
        directory: staging_directory.clone(),
    };
    validate_generation(&staged_generation)?;
    fs::rename(&staging_directory, &directory)?;

    let generation = ProjectIndexGeneration {
        deployment_id: deployment_id.to_owned(),
        generation_id,
        project_id: project_id.to_owned(),
        directory,
    };
    // Reopen at the final immutable path: a generation is not returned to the
    // caller unless it is verifiably complete at the path a registry will use.
    validate_generation(&generation)?;
    Ok(generation)
}

fn write_documents(
    index: &Index,
    fields: SearchFields,
    project_id: &str,
    deployment_id: &str,
    documents: &[SearchDocument],
    tokenizers: &TokenizerHealth,
) -> Result<(), SearchError> {
    let mut writer = index.writer(50_000_000)?;
    for document in documents {
        let checksum = document_checksum(document);
        let mut indexed = doc!(
            fields.project_id => project_id,
            fields.deployment_id => deployment_id,
            fields.page_id => document.page_id.as_str(),
            fields.language => document.language.as_str(),
            fields.path => document.path.as_str(),
            fields.title => document.title.as_str(),
            fields.document_checksum => checksum.as_str(),
        );
        indexed.add_text(fields.version_id, &document.version_id);
        indexed.add_text(fields.version_slug, &document.version_slug);
        indexed.add_text(fields.description, &document.description);
        indexed.add_text(fields.headings, &document.headings);
        indexed.add_text(fields.source_content, &document.content);
        if let Some(icon) = &document.icon {
            indexed.add_text(fields.icon, icon);
        }
        let analyzed = fields.analyzed(&document.language);
        indexed.add_text(analyzed.title, &document.title);
        indexed.add_text(analyzed.headings, &document.headings);
        indexed.add_text(analyzed.description, &document.description);
        // Retain authored Markdown/MDX text for lexical recall. The
        // CommonMark snippet projection is not an MDX renderer and must
        // not silently erase text inside authoring components at indexing.
        indexed.add_text(analyzed.content, &document.content);
        writer.add_document(indexed)?;
    }
    writer.commit()?;
    // A successful immutable build must not leave a merge writing into the
    // staging path after validation/rename. commit() joins indexing workers;
    // this also joins the engine's remaining merge/indexing lifecycle.
    writer.wait_merging_threads()?;
    // A failed infallible token stream cannot reach manifest creation or
    // generation publication, even though Tantivy itself accepted the commit.
    tokenizers.check()
}

/// Opens and fully validates an immutable generation at its canonical scoped
/// path. Promotion code must use this validation gate before atomically making
/// a generation active in PostgreSQL.
///
/// # Errors
/// Returns an error when identifiers are invalid, the final generation is
/// absent, its manifest is malformed, or the index and manifest do not agree.
pub fn open_generation(
    index_root: &Path,
    project_id: &str,
    deployment_id: &str,
    generation_id: Uuid,
) -> Result<ProjectIndexGeneration, SearchError> {
    open_generation_with_manifest(index_root, project_id, deployment_id, generation_id)
        .map(|(generation, _)| generation)
}

fn open_generation_with_manifest(
    index_root: &Path,
    project_id: &str,
    deployment_id: &str,
    generation_id: Uuid,
) -> Result<(ProjectIndexGeneration, GenerationManifest), SearchError> {
    open_generation_with_budget(index_root, project_id, deployment_id, generation_id, None)
}

fn open_generation_with_budget(
    index_root: &Path,
    project_id: &str,
    deployment_id: &str,
    generation_id: Uuid,
    budget: Option<&QueryBudget>,
) -> Result<(ProjectIndexGeneration, GenerationManifest), SearchError> {
    checkpoint(budget)?;
    validate_identifier(project_id)?;
    validate_identifier(deployment_id)?;
    let generation = ProjectIndexGeneration {
        deployment_id: deployment_id.to_owned(),
        generation_id,
        project_id: project_id.to_owned(),
        directory: generation_directory(index_root, project_id, generation_id),
    };
    if !generation.directory().is_dir() {
        return Err(SearchError::Integrity("generation directory is missing"));
    }
    let manifest = validate_generation_with_budget(&generation, budget)?;
    Ok((generation, manifest))
}

/// Reads the manifest, checks its identity and checksums, then scans the index
/// to establish that every live stored document belongs to the generation and
/// exactly matches the manifest's per-page checksum map. This is deliberately
/// a full O(generation size) integrity proof. Builders use the unrestricted
/// gate; the API query runtime uses the guarded variant with operational caps
/// and cooperative checkpoints, not an unverified integrity cache.
///
/// # Errors
/// Returns an error for a missing/corrupt manifest, identity mismatch, schema
/// mismatch, document-count mismatch, duplicate page IDs, or a checksum/binding
/// mismatch.
pub fn validate_generation(
    generation: &ProjectIndexGeneration,
) -> Result<GenerationManifest, SearchError> {
    validate_generation_with_budget(generation, None)
}

fn validate_generation_with_budget(
    generation: &ProjectIndexGeneration,
    budget: Option<&QueryBudget>,
) -> Result<GenerationManifest, SearchError> {
    checkpoint(budget)?;
    validate_identifier(generation.project_id())?;
    validate_identifier(generation.deployment_id())?;
    let manifest = read_manifest_with_budget(generation.directory(), budget)?;
    if budget.is_some_and(|budget| manifest.document_count > budget.config.max_documents as u64) {
        return Err(SearchError::ResourceLimit("generation documents"));
    }
    validate_manifest(&manifest, generation)?;

    let index = Index::open_in_dir(generation.directory())?;
    let fields = fields(&index.schema())?;
    let reader = index
        .reader_builder()
        .reload_policy(tantivy::ReloadPolicy::Manual)
        .try_into()?;
    let searcher = reader.searcher();
    if searcher.num_docs() != manifest.document_count {
        return Err(SearchError::Integrity(
            "index document count does not match manifest",
        ));
    }
    let limit = usize::try_from(searcher.num_docs())
        .map_err(|_| SearchError::Integrity("index document count exceeds platform bounds"))?;
    let documents = if limit == 0 {
        Vec::new()
    } else {
        searcher.search(&AllQuery, &TopDocs::with_limit(limit).order_by_score())?
    };
    if documents.len() != limit {
        return Err(SearchError::Integrity(
            "index does not expose every document recorded by the manifest",
        ));
    }
    checkpoint(budget)?;
    let mut observed_checksums = BTreeMap::new();
    let mut source_bytes = 0_usize;
    for (_, address) in documents {
        checkpoint(budget)?;
        let document: TantivyDocument = searcher.doc(address)?;
        if stored_text(&document, fields.project_id)? != generation.project_id()
            || stored_text(&document, fields.deployment_id)? != generation.deployment_id()
        {
            return Err(SearchError::Integrity(
                "stored document does not bind to this project deployment",
            ));
        }
        let source = stored_document(&document, fields)?;
        source_bytes = source_bytes
            .checked_add(document_text_bytes(&source))
            .ok_or(SearchError::ResourceLimit("validated source bytes"))?;
        if budget.is_some_and(|budget| source_bytes > budget.config.max_source_bytes) {
            return Err(SearchError::ResourceLimit("validated source bytes"));
        }
        validate_document(&source)
            .map_err(|_| SearchError::Integrity("stored source is invalid"))?;
        let checksum = stored_text(&document, fields.document_checksum)?;
        if checksum != document_checksum(&source) {
            return Err(SearchError::Integrity(
                "stored source fields do not match their document checksum",
            ));
        }
        if observed_checksums
            .insert(source.page_id, checksum)
            .is_some()
        {
            return Err(SearchError::Integrity(
                "index contains duplicate page identifiers",
            ));
        }
    }
    checkpoint(budget)?;
    if observed_checksums != manifest.document_checksums {
        return Err(SearchError::Integrity(
            "stored document checksums do not match manifest",
        ));
    }
    Ok(manifest)
}

/// Returns a final path with only a digest of the project ID as a component.
/// This prevents a caller-controlled project identifier from creating traversal
/// or collision-prone directory names.
#[must_use]
pub fn generation_directory(index_root: &Path, project_id: &str, generation_id: Uuid) -> PathBuf {
    let project_digest = hex::encode(Sha256::digest(project_id.as_bytes()));
    index_root
        .join("projects")
        .join(project_digest)
        .join("generations")
        .join(generation_id.to_string())
}

fn staging_directory(index_root: &Path, project_id: &str, generation_id: Uuid) -> PathBuf {
    let directory = generation_directory(index_root, project_id, generation_id);
    directory.with_file_name(format!(".{}.building", generation_id.as_hyphenated()))
}

/// Removes only the canonical unpublishable staging directory for a generation.
/// A registry worker must call this only after it has fenced/reclaimed the
/// generation lease; this function never touches the final generation path.
///
/// # Errors
/// Returns an error for malformed identities, storage failures, or an
/// unexpected non-directory staging path.
pub fn discard_staging_generation(
    index_root: &Path,
    project_id: &str,
    generation_id: Uuid,
) -> Result<bool, SearchError> {
    validate_identifier(project_id)?;
    let directory = staging_directory(index_root, project_id, generation_id);
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(directory)?;
            Ok(true)
        }
        Ok(_) => Err(SearchError::Integrity(
            "generation staging path is not a directory",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(SearchError::Storage(error)),
    }
}

fn schema() -> Schema {
    let mut builder = Schema::builder();
    for name in [
        "project_id",
        "deployment_id",
        "page_id",
        "version_id",
        "version_slug",
        "language",
        "path",
        "document_checksum",
    ] {
        builder.add_text_field(name, STRING | STORED);
    }
    for name in ["title", "description", "icon", "headings", "source_content"] {
        builder.add_text_field(name, TextOptions::default().set_stored());
    }
    for (family, tokenizer) in [
        ("japanese", JAPANESE_TOKENIZER_NAME),
        ("chinese", CHINESE_TOKENIZER_NAME),
        ("general", "default"),
    ] {
        for component in ["title", "headings", "description", "content"] {
            builder.add_text_field(
                &format!("{family}_{component}"),
                indexed_text_options(tokenizer),
            );
        }
    }
    builder.build()
}

fn indexed_text_options(tokenizer: &str) -> TextOptions {
    TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(tokenizer)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    )
}

fn fields(candidate: &Schema) -> Result<SearchFields, SearchError> {
    // Format 2 is a deliberately closed schema. In particular an old index
    // lacking exact version/locale and rendering fields must never be treated
    // as public-compatible merely because a few field names still match.
    if candidate != &schema() {
        return Err(SearchError::Integrity(
            "generation schema requires a format-2 rebuild",
        ));
    }
    let field = |name: &str| candidate.get_field(name).map_err(SearchError::from);
    let analyzed = |family| -> Result<AnalyzedFields, SearchError> {
        Ok(AnalyzedFields {
            title: field(&format!("{family}_title"))?,
            headings: field(&format!("{family}_headings"))?,
            description: field(&format!("{family}_description"))?,
            content: field(&format!("{family}_content"))?,
        })
    };
    Ok(SearchFields {
        project_id: field("project_id")?,
        deployment_id: field("deployment_id")?,
        page_id: field("page_id")?,
        version_id: field("version_id")?,
        version_slug: field("version_slug")?,
        language: field("language")?,
        path: field("path")?,
        title: field("title")?,
        description: field("description")?,
        icon: field("icon")?,
        headings: field("headings")?,
        source_content: field("source_content")?,
        document_checksum: field("document_checksum")?,
        japanese: analyzed("japanese")?,
        chinese: analyzed("chinese")?,
        general: analyzed("general")?,
    })
}

fn register_lindera(index: &Index) -> Result<TokenizerHealth, SearchError> {
    let health = TokenizerHealth::default();
    index.tokenizers().register(
        JAPANESE_TOKENIZER_NAME,
        LinderaTokenizer::load("embedded://ipadic", health.clone())?,
    );
    index.tokenizers().register(
        CHINESE_TOKENIZER_NAME,
        LinderaTokenizer::load("embedded://cc-cedict", health.clone())?,
    );
    Ok(health)
}

fn manifest_for(
    project_id: &str,
    deployment_id: &str,
    generation_id: Uuid,
    documents: &[SearchDocument],
) -> Result<GenerationManifest, SearchError> {
    let document_checksums = document_checksums_for(documents)?;
    let digest = digest_for_checksums(&document_checksums)?;
    Ok(GenerationManifest {
        format_version: MANIFEST_FORMAT_VERSION,
        runtime: runtime::current_runtime(),
        generation_id,
        project_id: project_id.to_owned(),
        deployment_id: deployment_id.to_owned(),
        document_count: digest.document_count,
        documents_sha256: digest.documents_sha256,
        document_checksums,
    })
}

/// Calculates the exact content identity a generation manifest will carry for
/// a deployment's searchable documents. The digest is independent of input
/// slice order, but rejects duplicate page IDs rather than choosing one.
///
/// # Errors
/// Returns an error for malformed document fields or a duplicate page ID. An
/// empty set is valid: a project containing only navigation groups still needs
/// a promoted, immutable generation so publication availability does not hinge
/// on having a searchable page.
pub fn documents_digest(documents: &[SearchDocument]) -> Result<DocumentDigest, SearchError> {
    digest_for_checksums(&document_checksums_for(documents)?)
}

fn document_checksums_for(
    documents: &[SearchDocument],
) -> Result<BTreeMap<String, String>, SearchError> {
    let mut document_checksums = BTreeMap::new();
    for document in documents {
        validate_document(document)?;
        let previous =
            document_checksums.insert(document.page_id.clone(), document_checksum(document));
        if previous.is_some() {
            return Err(SearchError::InvalidInput(
                "a generation cannot contain duplicate page identifiers",
            ));
        }
    }
    Ok(document_checksums)
}

fn digest_for_checksums(
    document_checksums: &BTreeMap<String, String>,
) -> Result<DocumentDigest, SearchError> {
    let document_count = u64::try_from(document_checksums.len())
        .map_err(|_| SearchError::Integrity("document count exceeds manifest bounds"))?;
    Ok(DocumentDigest {
        document_count,
        documents_sha256: checksums_digest(document_checksums),
    })
}

fn write_manifest(directory: &Path, manifest: &GenerationManifest) -> Result<(), SearchError> {
    let manifest_path = directory.join(MANIFEST_FILE_NAME);
    if manifest_path.exists() {
        return Err(SearchError::Integrity("generation manifest already exists"));
    }
    let bytes = serde_json::to_vec(manifest)?;
    let temporary_path = directory.join(format!(".{MANIFEST_FILE_NAME}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary_path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary_path, manifest_path)?;
    Ok(())
}

#[cfg(test)]
fn read_manifest(directory: &Path) -> Result<GenerationManifest, SearchError> {
    read_manifest_with_budget(directory, None)
}

fn read_manifest_with_budget(
    directory: &Path,
    budget: Option<&QueryBudget>,
) -> Result<GenerationManifest, SearchError> {
    checkpoint(budget)?;
    let path = directory.join(MANIFEST_FILE_NAME);
    let bytes = if let Some(budget) = budget {
        let file = fs::File::open(path)?;
        let maximum = budget.config.max_manifest_bytes;
        if file.metadata()?.len() > maximum as u64 {
            return Err(SearchError::ResourceLimit("generation manifest bytes"));
        }
        let mut bytes = Vec::new();
        file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > maximum {
            return Err(SearchError::ResourceLimit("generation manifest bytes"));
        }
        bytes
    } else {
        fs::read(path)?
    };
    checkpoint(budget)?;
    let manifest = serde_json::from_slice(&bytes)?;
    checkpoint(budget)?;
    Ok(manifest)
}

fn document_text_bytes(document: &SearchDocument) -> usize {
    [
        document.page_id.as_str(),
        &document.language,
        &document.version_id,
        &document.version_slug,
        &document.path,
        &document.title,
        &document.description,
        document.icon.as_deref().unwrap_or_default(),
        &document.headings,
        &document.content,
    ]
    .into_iter()
    .map(str::len)
    .sum()
}

fn validate_manifest_runtime(manifest: &GenerationManifest) -> Result<(), SearchError> {
    if manifest.format_version != MANIFEST_FORMAT_VERSION {
        return Err(SearchError::Integrity(
            "generation manifest format is unsupported",
        ));
    }
    manifest.runtime.ensure_current()
}

fn validate_manifest(
    manifest: &GenerationManifest,
    generation: &ProjectIndexGeneration,
) -> Result<(), SearchError> {
    validate_manifest_runtime(manifest)?;
    if manifest.generation_id != generation.generation_id()
        || manifest.project_id != generation.project_id()
        || manifest.deployment_id != generation.deployment_id()
    {
        return Err(SearchError::Integrity(
            "generation manifest identity does not match",
        ));
    }
    validate_identifier(&manifest.project_id)?;
    validate_identifier(&manifest.deployment_id)?;
    let checksum_count = u64::try_from(manifest.document_checksums.len())
        .map_err(|_| SearchError::Integrity("manifest document count exceeds platform bounds"))?;
    if manifest.document_count != checksum_count {
        return Err(SearchError::Integrity("manifest document count is invalid"));
    }
    if manifest
        .document_checksums
        .iter()
        .any(|(page_id, checksum)| {
            validate_identifier(page_id).is_err() || !is_sha256_hex(checksum)
        })
    {
        return Err(SearchError::Integrity(
            "manifest document checksum entry is invalid",
        ));
    }
    if !is_sha256_hex(&manifest.documents_sha256)
        || manifest.documents_sha256 != checksums_digest(&manifest.document_checksums)
    {
        return Err(SearchError::Integrity(
            "manifest aggregate document checksum is invalid",
        ));
    }
    Ok(())
}

fn document_checksum(document: &SearchDocument) -> String {
    let mut hasher = Sha256::new();
    update_checksum_component(&mut hasher, b"nibleaf-search-document-v2");
    for component in [
        document.page_id.as_bytes(),
        document.language.as_bytes(),
        document.version_id.as_bytes(),
        document.version_slug.as_bytes(),
        document.path.as_bytes(),
        document.title.as_bytes(),
        document.description.as_bytes(),
        document.headings.as_bytes(),
        document.content.as_bytes(),
    ] {
        update_checksum_component(&mut hasher, component);
    }
    // Encode option presence separately: no icon and an explicitly empty icon
    // are different immutable source facts.
    match &document.icon {
        Some(icon) => {
            update_checksum_component(&mut hasher, b"some");
            update_checksum_component(&mut hasher, icon.as_bytes());
        }
        None => update_checksum_component(&mut hasher, b"none"),
    }
    hex::encode(hasher.finalize())
}

fn checksums_digest(checksums: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    update_checksum_component(&mut hasher, b"nibleaf-search-generation-documents-v2");
    for (page_id, checksum) in checksums {
        update_checksum_component(&mut hasher, page_id.as_bytes());
        update_checksum_component(&mut hasher, checksum.as_bytes());
    }
    hex::encode(hasher.finalize())
}

fn update_checksum_component(hasher: &mut Sha256, component: &[u8]) {
    hasher.update(
        u64::try_from(component.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    hasher.update(component);
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[derive(Clone, Copy)]
enum LanguageFamily {
    Chinese,
    General,
    Japanese,
}

fn language_family(language: &str) -> LanguageFamily {
    let normalized = language.to_ascii_lowercase();
    if normalized == "ja" || normalized.starts_with("ja-") {
        LanguageFamily::Japanese
    } else if normalized == "zh" || normalized.starts_with("zh-") {
        LanguageFamily::Chinese
    } else {
        LanguageFamily::General
    }
}

fn stored_optional_text(
    document: &TantivyDocument,
    field: Field,
) -> Result<Option<String>, SearchError> {
    let mut values = document.get_all(field);
    let first = values.next();
    if values.next().is_some() {
        return Err(SearchError::Integrity(
            "stored index field has duplicate values",
        ));
    }
    first
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or(SearchError::Integrity("stored index field is not text"))
        })
        .transpose()
}

fn stored_text(document: &TantivyDocument, field: Field) -> Result<String, SearchError> {
    stored_optional_text(document, field)?
        .ok_or(SearchError::Integrity("stored index document is malformed"))
}

fn stored_document(
    document: &TantivyDocument,
    fields: SearchFields,
) -> Result<SearchDocument, SearchError> {
    Ok(SearchDocument {
        page_id: stored_text(document, fields.page_id)?,
        language: stored_text(document, fields.language)?,
        version_id: stored_text(document, fields.version_id)?,
        version_slug: stored_text(document, fields.version_slug)?,
        path: stored_text(document, fields.path)?,
        title: stored_text(document, fields.title)?,
        description: stored_text(document, fields.description)?,
        icon: stored_optional_text(document, fields.icon)?,
        headings: stored_text(document, fields.headings)?,
        content: stored_text(document, fields.source_content)?,
    })
}

fn validate_identifier(value: &str) -> Result<(), SearchError> {
    if value.is_empty() || value.len() > 191 || value.trim() != value {
        return Err(SearchError::InvalidInput(
            "identifier is empty, oversized, or padded",
        ));
    }
    Ok(())
}

fn validate_document(document: &SearchDocument) -> Result<(), SearchError> {
    for value in [
        &document.page_id,
        &document.language,
        &document.version_id,
        &document.version_slug,
    ] {
        validate_identifier(value)?;
    }
    for value in [&document.path, &document.title] {
        if value.is_empty() || value.trim() != value || value.len() > MAX_DOCUMENT_TEXT_BYTES {
            return Err(SearchError::InvalidInput("page title or path is invalid"));
        }
    }
    for value in [
        document.content.as_str(),
        document.description.as_str(),
        document.headings.as_str(),
        document.icon.as_deref().unwrap_or_default(),
    ] {
        if value.len() > MAX_DOCUMENT_TEXT_BYTES {
            return Err(SearchError::InvalidInput(
                "document text exceeds its byte bound",
            ));
        }
    }
    // A titled page can intentionally have no body or description. The exact
    // empty fields still participate in the immutable checksum.
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{env, fs};

    use super::*;

    fn test_root() -> PathBuf {
        env::temp_dir().join(format!("nibleaf-search-test-{}", Uuid::now_v7()))
    }

    fn page(language: &str, id: &str, title: &str, content: &str) -> SearchDocument {
        SearchDocument {
            page_id: id.to_owned(),
            language: language.to_owned(),
            version_id: "branch-main".to_owned(),
            version_slug: "main".to_owned(),
            description: String::new(),
            icon: None,
            headings: String::new(),
            path: format!("/{id}"),
            title: title.to_owned(),
            content: content.to_owned(),
        }
    }

    #[test]
    fn project_generations_are_path_safe_and_isolated() -> Result<(), SearchError> {
        let root = test_root();
        let first = build_generation(
            &root,
            "project-a",
            "deployment-a",
            Uuid::now_v7(),
            &[page(
                "en",
                "page-a",
                "Airport",
                "Tokyo international airport guide",
            )],
        )?;
        let second = build_generation(
            &root,
            "project-b",
            "deployment-b",
            Uuid::now_v7(),
            &[page("en", "page-b", "Harbor", "Harbor operations guide")],
        )?;
        assert_ne!(
            first.manifest_path().parent(),
            second.manifest_path().parent()
        );
        assert!(
            !first
                .manifest_path()
                .to_string_lossy()
                .contains("project-a")
        );
        assert!(
            search_generation(&first, "Tokyo", 10)?
                .iter()
                .all(|hit| hit.page_id == "page-a")
        );
        assert_eq!(search_generation(&second, "Tokyo", 10)?, Vec::new());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn empty_generation_is_valid_for_navigation_only_projects() -> Result<(), SearchError> {
        let root = test_root();
        let generation = build_generation(
            &root,
            "project-navigation-only",
            "deployment-navigation-only",
            Uuid::now_v7(),
            &[],
        )?;
        let manifest = validate_generation(&generation)?;
        assert_eq!(manifest.document_count, 0);
        assert!(manifest.document_checksums.is_empty());
        assert_eq!(search_generation(&generation, "anything", 10)?, Vec::new());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn manifest_is_immutable_evidence_and_is_verified_on_open() -> Result<(), SearchError> {
        let root = test_root();
        let generation_id = Uuid::now_v7();
        let generation = build_generation(
            &root,
            "project-manifest",
            "deployment-manifest",
            generation_id,
            &[
                page("en", "first", "First page", "First page body"),
                page("en", "second", "Second page", "Second page body"),
            ],
        )?;
        let manifest = validate_generation(&generation)?;
        assert_eq!(manifest.generation_id, generation_id);
        assert_eq!(manifest.document_count, 2);
        assert_eq!(manifest.document_checksums.len(), 2);
        assert!(is_sha256_hex(&manifest.documents_sha256));
        let reopened = open_generation(
            &root,
            "project-manifest",
            "deployment-manifest",
            generation_id,
        )?;
        assert_eq!(reopened, generation);

        let mut corrupted_identity = manifest.clone();
        corrupted_identity.project_id = "project-other".to_owned();
        fs::write(
            generation.manifest_path(),
            serde_json::to_vec(&corrupted_identity)?,
        )?;
        assert!(matches!(
            open_generation(
                &root,
                "project-manifest",
                "deployment-manifest",
                generation_id
            ),
            Err(SearchError::Integrity(_))
        ));

        let mut corrupted_checksum = manifest;
        corrupted_checksum
            .document_checksums
            .insert("first".to_owned(), "0".repeat(64));
        corrupted_checksum.documents_sha256 =
            checksums_digest(&corrupted_checksum.document_checksums);
        fs::write(
            generation.manifest_path(),
            serde_json::to_vec(&corrupted_checksum)?,
        )?;
        assert!(matches!(
            open_generation(
                &root,
                "project-manifest",
                "deployment-manifest",
                generation_id
            ),
            Err(SearchError::Integrity(_))
        ));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn source_digest_binds_every_field_and_manifest_refuses_old_formats() -> Result<(), SearchError>
    {
        validate_document(&page("en", "long-title", &"T".repeat(200), ""))?;
        let original = page("en", "page", "Title", "Content");
        let expected = document_checksum(&original);
        let mut variants = Vec::new();
        let mut changed = original.clone();
        changed.version_id = "other-branch".to_owned();
        variants.push(changed);
        let mut changed = original.clone();
        changed.version_slug = "other-version".to_owned();
        variants.push(changed);
        let mut changed = original.clone();
        changed.language = "en-GB".to_owned();
        variants.push(changed);
        let mut changed = original.clone();
        changed.description = "Summary".to_owned();
        variants.push(changed);
        let mut changed = original.clone();
        changed.icon = Some(String::new());
        variants.push(changed);
        let mut changed = original.clone();
        changed.headings = "Heading".to_owned();
        variants.push(changed);
        for changed in variants {
            assert_ne!(document_checksum(&changed), expected);
        }
        let generation_id = Uuid::now_v7();
        let generation = ProjectIndexGeneration {
            deployment_id: "deployment-a".to_owned(),
            generation_id,
            project_id: "project-a".to_owned(),
            directory: PathBuf::new(),
        };
        let mut manifest = manifest_for("project-a", "deployment-a", generation_id, &[original])?;
        assert_eq!(manifest.format_version, 3);
        validate_manifest(&manifest, &generation)?;
        for old_format in [1, 2] {
            manifest.format_version = old_format;
            assert!(matches!(
                validate_manifest(&manifest, &generation),
                Err(SearchError::Integrity(_))
            ));
        }
        Ok(())
    }

    #[test]
    fn validation_recomputes_stored_source_not_just_a_copied_checksum() -> Result<(), SearchError> {
        use tantivy::Term;
        let root = test_root();
        let generation = build_generation(
            &root,
            "project-a",
            "deployment-a",
            Uuid::now_v7(),
            &[page("en", "page", "Title", "Content")],
        )?;
        let index = Index::open_in_dir(generation.directory())?;
        let fields = fields(&index.schema())?;
        let reader = index.reader()?;
        let searcher = reader.searcher();
        let address = searcher.search(&AllQuery, &TopDocs::with_limit(1).order_by_score())?[0].1;
        let stored: TantivyDocument = searcher.doc(address)?;
        let mut tampered = TantivyDocument::default();
        for (field, value) in stored.field_values() {
            if field != fields.path {
                tampered.add_field_value(field, value);
            }
        }
        tampered.add_text(fields.path, "tampered-path");
        let tokenizers = register_lindera(&index)?;
        let mut writer = index.writer(50_000_000)?;
        writer.delete_term(Term::from_field_text(fields.page_id, "page"));
        writer.add_document(tampered)?;
        writer.commit()?;
        tokenizers.check()?;
        drop(writer);
        assert!(matches!(
            validate_generation(&generation),
            Err(SearchError::Integrity(_))
        ));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn writer_commit_cannot_hide_a_segmentation_failure() -> Result<(), SearchError> {
        let index = Index::builder().schema(schema()).create_in_ram()?;
        let health = tokenizer::tests::install_failing_tokenizers(&index);
        let fields = fields(&index.schema())?;
        assert!(matches!(
            write_documents(
                &index,
                fields,
                "project-failure",
                "deployment-failure",
                &[page("ja", "page", "東京", "文書")],
                &health,
            ),
            Err(SearchError::Tokenizer(_))
        ));
        Ok(())
    }

    #[test]
    fn opening_refuses_changed_runtime_and_missing_old_manifest_evidence() -> Result<(), SearchError>
    {
        let root = test_root();
        let generation_id = Uuid::now_v7();
        let generation = build_generation(
            &root,
            "project-runtime",
            "deployment-runtime",
            generation_id,
            &[page("en", "page", "Title", "Content")],
        )?;
        let path = generation.directory().join(MANIFEST_FILE_NAME);
        let original = serde_json::to_value(read_manifest(generation.directory())?)?;
        let mut changed = original.clone();
        changed["runtime"]["lindera"] = serde_json::json!("2.3.4");
        fs::write(&path, serde_json::to_vec(&changed)?)?;
        assert!(matches!(
            open_generation(
                &root,
                "project-runtime",
                "deployment-runtime",
                generation_id
            ),
            Err(SearchError::Integrity(_))
        ));
        for format in [2, 3] {
            let mut missing_runtime = original.clone();
            missing_runtime["format_version"] = serde_json::json!(format);
            if let serde_json::Value::Object(ref mut object) = missing_runtime {
                object.remove("runtime");
            }
            fs::write(&path, serde_json::to_vec(&missing_runtime)?)?;
            assert!(matches!(
                open_generation(
                    &root,
                    "project-runtime",
                    "deployment-runtime",
                    generation_id
                ),
                Err(SearchError::Manifest(_))
            ));
        }
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn lindera_indexes_japanese_terms() -> Result<(), SearchError> {
        let root = test_root();
        let generation = build_generation(
            &root,
            "project-ja",
            "deployment-ja",
            Uuid::now_v7(),
            &[page(
                "ja",
                "tokyo",
                "東京国際空港",
                "東京国際空港へのアクセス案内",
            )],
        )?;
        let hits = search_generation(&generation, "東京", 10)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page_id, "tokyo");
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn lindera_indexes_chinese_terms_with_a_language_scoped_query() -> Result<(), SearchError> {
        let root = test_root();
        let generation = build_generation(
            &root,
            "project-zh",
            "deployment-zh",
            Uuid::now_v7(),
            &[page(
                "zh-CN",
                "beijing",
                "北京国际机场",
                "北京国际机场交通指南",
            )],
        )?;
        let hits = search_generation_for_language(&generation, "北京", Some("zh-CN"), 10)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page_id, "beijing");
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
