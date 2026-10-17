//! Lexical query execution with mandatory pre-collector scope filters.

#[cfg(test)]
mod public_tests;
mod scoped_rank;

use std::collections::BTreeSet;

use tantivy::{
    Index, TantivyDocument, Term,
    collector::TopDocs,
    query::{
        BooleanQuery, BoostQuery, ConstScoreQuery, EmptyQuery, Occur, Query, QueryParser,
        TermQuery, TermSetQuery,
    },
    schema::{Field, IndexRecordOption},
};

use crate::{
    AnalyzedFields, MAX_QUERY_BYTES, MAX_RESULTS, ProjectIndexGeneration, PublishedSearchAdmission,
    SearchError, SearchFields, SearchHit, TokenizerHealth,
    execution::budget::{QueryBudget, checkpoint},
    fields, register_lindera, stored_document, stored_text, text, validate_identifier,
};

#[derive(Default)]
struct Filters<'a> {
    language: Option<&'a str>,
    version_id: Option<&'a str>,
    version_slug: Option<&'a str>,
    allowed_page_ids: Option<&'a BTreeSet<String>>,
    allowed_languages: Option<&'a BTreeSet<String>>,
    public_literals: bool,
    scoped_ranking: bool,
}

impl Filters<'_> {
    fn permits(&self, source: &crate::SearchDocument) -> bool {
        self.language.is_none_or(|value| value == source.language)
            && self
                .version_id
                .is_none_or(|value| value == source.version_id)
            && self
                .version_slug
                .is_none_or(|value| value == source.version_slug)
            && self
                .allowed_page_ids
                .is_none_or(|ids| ids.contains(&source.page_id))
            && self
                .allowed_languages
                .is_none_or(|languages| languages.contains(&source.language))
    }
}

/// Searches one known project generation across all its eligible versions and
/// enabled languages. This unscoped convenience API is for trusted management
/// callers only; public readers must use [`search_admitted_generation`].
///
/// # Errors
/// Returns an error for invalid bounds, query syntax, index/storage failure, or
/// a corrupt project/deployment/document binding.
pub fn search_generation(
    generation: &ProjectIndexGeneration,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, SearchError> {
    search_generation_for_language(generation, query, None, limit)
}

/// Searches one generation with an optional **exact** language-code filter.
/// The tokenizer family only chooses analysis; `en` cannot match `fr`, nor can
/// `zh-CN` match `zh-TW`. No language means all indexed enabled languages.
///
/// # Errors
/// Returns the same errors as [`search_generation`].
pub fn search_generation_for_language(
    generation: &ProjectIndexGeneration,
    query: &str,
    language: Option<&str>,
    limit: usize,
) -> Result<Vec<SearchHit>, SearchError> {
    search_generation_for_version(generation, query, None, language, limit)
}

/// Trusted member-management query with optional exact version slug and locale.
/// An unknown slug/code produces no hits; public fallback/default resolution
/// must instead use the selected immutable deployment's version/language list.
///
/// # Errors
/// Returns the same errors as [`search_generation`].
pub fn search_generation_for_version(
    generation: &ProjectIndexGeneration,
    query: &str,
    version_slug: Option<&str>,
    language: Option<&str>,
    limit: usize,
) -> Result<Vec<SearchHit>, SearchError> {
    search_filtered(
        generation,
        query,
        &Filters {
            language,
            version_slug,
            ..Filters::default()
        },
        limit,
    )
}

/// Non-HTTP execution primitive for a freshly admitted public reader. It binds
/// admission to the generation's project and applies the required exact opaque
/// version ID, optional exact locale, and server-derived page grants **inside
/// Tantivy before `TopDocs`**. No browser-supplied page list is accepted.
///
/// The caller must select the deployment/generation and resolve the version and
/// enabled-language defaults from that same immutable snapshot. This function
/// does not select publications, reauthenticate a stale admission, emit
/// analytics, or expose a public route. Use a joined blocking task in async
/// code, and derive admission anew for each request.
///
/// # Errors
/// Returns an integrity error before opening an index if admission belongs to
/// another project; otherwise returns the same errors as [`search_generation`].
pub fn search_admitted_generation(
    generation: &ProjectIndexGeneration,
    admission: &PublishedSearchAdmission,
    query: &str,
    version_id: &str,
    language: Option<&str>,
    limit: usize,
) -> Result<Vec<SearchHit>, SearchError> {
    if admission.project_id() != generation.project_id() {
        return Err(SearchError::Integrity(
            "admission belongs to another project",
        ));
    }
    search_filtered(
        generation,
        query,
        &Filters {
            language,
            version_id: Some(version_id),
            allowed_page_ids: admission.allowed_page_ids(),
            scoped_ranking: true,
            ..Filters::default()
        },
        limit,
    )
}

// Only the server-selected publication composition supplies this language
// allow-list. An omitted client locale must not bypass a live disable flag.
pub(crate) fn search_published_generation_with_budget(
    generation: &ProjectIndexGeneration,
    admission: &PublishedSearchAdmission,
    query: &str,
    version_id: &str,
    languages: &BTreeSet<String>,
    limit: usize,
    budget: Option<&QueryBudget>,
) -> Result<Vec<SearchHit>, SearchError> {
    if admission.project_id() != generation.project_id() {
        return Err(SearchError::Integrity(
            "admission belongs to another project",
        ));
    }
    let language = if languages.len() == 1 {
        languages.first().map(String::as_str)
    } else {
        None
    };
    search_filtered_with_budget(
        generation,
        query,
        &Filters {
            language,
            version_id: Some(version_id),
            allowed_page_ids: admission.allowed_page_ids(),
            allowed_languages: Some(languages),
            public_literals: true,
            ..Filters::default()
        },
        limit,
        budget,
    )
}

pub(crate) fn search_member_with_budget(
    generation: &ProjectIndexGeneration,
    query: &str,
    version_slug: Option<&str>,
    language: Option<&str>,
    limit: usize,
    budget: &QueryBudget,
) -> Result<Vec<SearchHit>, SearchError> {
    search_filtered_with_budget(
        generation,
        query,
        &Filters {
            language,
            version_slug,
            ..Filters::default()
        },
        limit,
        Some(budget),
    )
}

fn search_filtered(
    generation: &ProjectIndexGeneration,
    query: &str,
    filters: &Filters<'_>,
    limit: usize,
) -> Result<Vec<SearchHit>, SearchError> {
    search_filtered_with_budget(generation, query, filters, limit, None)
}

fn search_filtered_with_budget(
    generation: &ProjectIndexGeneration,
    query: &str,
    filters: &Filters<'_>,
    limit: usize,
    budget: Option<&QueryBudget>,
) -> Result<Vec<SearchHit>, SearchError> {
    checkpoint(budget)?;
    validate_identifier(generation.project_id())?;
    for value in [filters.language, filters.version_id, filters.version_slug]
        .into_iter()
        .flatten()
    {
        validate_identifier(value)?;
    }
    let max_bytes = if filters.public_literals {
        600
    } else {
        MAX_QUERY_BYTES
    };
    if (query.trim().is_empty() && !filters.public_literals) || query.len() > max_bytes {
        return Err(SearchError::InvalidInput(
            "query must be non-empty and at most 512 bytes",
        ));
    }
    if !(1..=MAX_RESULTS).contains(&limit) {
        return Err(SearchError::InvalidInput(
            "result limit must be between 1 and 50",
        ));
    }
    if (filters.public_literals && query.trim().is_empty())
        || filters.allowed_page_ids.is_some_and(BTreeSet::is_empty)
        || filters.allowed_languages.is_some_and(BTreeSet::is_empty)
    {
        return Ok(Vec::new());
    }
    if let Some(languages) = filters.allowed_languages {
        for language in languages {
            validate_identifier(language)?;
        }
    }

    let index = Index::open_in_dir(generation.directory())?;
    let tokenizers = register_lindera(&index)?;
    let fields = fields(&index.schema())?;
    let reader = index
        .reader_builder()
        .reload_policy(tantivy::ReloadPolicy::Manual)
        .try_into()?;
    let searcher = reader.searcher();
    let scoped = scoped_query(&index, fields, generation, query, filters, &tokenizers)?;
    let top_documents = if filters.public_literals || filters.scoped_ranking {
        scoped_rank::search(
            &index,
            &searcher,
            fields,
            filters,
            &scoped,
            scoped_rank::RankingOptions {
                health: &tokenizers,
                budget,
            },
            limit,
        )?
    } else {
        searcher.search(&scoped, &TopDocs::with_limit(limit).order_by_score())?
    };
    checkpoint(budget)?;
    let mut hits = Vec::with_capacity(top_documents.len());
    for (score, address) in top_documents {
        checkpoint(budget)?;
        let document: TantivyDocument = searcher.doc(address)?;
        if stored_text(&document, fields.project_id)? != generation.project_id()
            || stored_text(&document, fields.deployment_id)? != generation.deployment_id()
        {
            return Err(SearchError::Integrity(
                "generation project/deployment binding is invalid",
            ));
        }
        let source = stored_document(&document, fields)?;
        // Defense in depth: a damaged posting/stored-field pair is unavailable,
        // never a justification for sending a result outside the admitted set.
        if !filters.permits(&source) {
            return Err(SearchError::Integrity(
                "result violates its exact search scope",
            ));
        }
        hits.push(SearchHit {
            score,
            snippet: text::snippet(&source.content, query),
            page_id: source.page_id,
            language: source.language,
            version_id: source.version_id,
            version_slug: source.version_slug,
            path: source.path,
            title: source.title,
            description: source.description,
            icon: source.icon,
        });
    }
    hits.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.page_id.cmp(&right.page_id))
    });
    checkpoint(budget)?;
    Ok(hits)
}

fn scoped_query(
    index: &Index,
    fields: SearchFields,
    generation: &ProjectIndexGeneration,
    query: &str,
    filters: &Filters<'_>,
    tokenizers: &TokenizerHealth,
) -> Result<BooleanQuery, SearchError> {
    let parsed = if filters.public_literals {
        literal_query(index, fields, filters.language, query)
    } else {
        parser(index, fields, filters.language)
            .parse_query(query)
            .map_err(SearchError::from)
    };
    // A tokenizer error is unavailability, even if the empty internal stream
    // also made parsing fail. Never report it as invalid caller syntax.
    tokenizers.check()?;
    let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![
        (Occur::Must, parsed?),
        (
            Occur::Must,
            exact(fields.project_id, generation.project_id()),
        ),
        (
            Occur::Must,
            exact(fields.deployment_id, generation.deployment_id()),
        ),
    ];
    for (field, value) in [
        (fields.language, filters.language),
        (fields.version_id, filters.version_id),
        (fields.version_slug, filters.version_slug),
    ] {
        if let Some(value) = value {
            clauses.push((Occur::Must, exact(field, value)));
        }
    }
    if let Some(languages) = filters.allowed_languages {
        let terms = languages
            .iter()
            .map(|language| Term::from_field_text(fields.language, language));
        clauses.push((
            Occur::Must,
            Box::new(ConstScoreQuery::new(
                Box::new(TermSetQuery::new(terms)),
                0.0,
            )),
        ));
    }
    if let Some(page_ids) = filters.allowed_page_ids {
        let terms = page_ids
            .iter()
            .map(|id| Term::from_field_text(fields.page_id, id));
        clauses.push((
            Occur::Must,
            Box::new(ConstScoreQuery::new(
                Box::new(TermSetQuery::new(terms)),
                0.0,
            )),
        ));
    }
    Ok(BooleanQuery::new(clauses))
}

// Public text is not Tantivy syntax: field names, regex/ranges and operators
// cannot probe another field/corpus or trigger dictionary-wide expansion.
fn literal_query(
    index: &Index,
    fields: SearchFields,
    language: Option<&str>,
    text: &str,
) -> Result<Box<dyn Query>, SearchError> {
    let families = language.map_or_else(
        || vec![fields.general, fields.japanese, fields.chinese],
        |language| vec![fields.analyzed(language)],
    );
    let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
    let mut seen = BTreeSet::new();
    for family in families {
        for (field, boost) in [
            (family.title, 4.0),
            (family.headings, 2.0),
            (family.description, 1.5),
            (family.content, 1.0),
        ] {
            let mut analyzer = index.tokenizer_for_field(field)?;
            let mut stream = analyzer.token_stream(text);
            while stream.advance() {
                let term = Term::from_field_text(field, &stream.token().text);
                if seen.insert(term.clone()) {
                    clauses.push((
                        Occur::Should,
                        Box::new(BoostQuery::new(
                            Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs)),
                            boost,
                        )),
                    ));
                }
            }
        }
    }
    if clauses.is_empty() {
        Ok(Box::new(EmptyQuery))
    } else {
        Ok(Box::new(BooleanQuery::new(clauses)))
    }
}

fn exact(field: Field, value: &str) -> Box<dyn Query> {
    Box::new(ConstScoreQuery::new(
        Box::new(TermQuery::new(
            Term::from_field_text(field, value),
            IndexRecordOption::Basic,
        )),
        0.0,
    ))
}

fn parser(index: &Index, fields: SearchFields, language: Option<&str>) -> QueryParser {
    let families = language.map_or_else(
        || vec![fields.general, fields.japanese, fields.chinese],
        |language| vec![fields.analyzed(language)],
    );
    let defaults = families
        .iter()
        .flat_map(|family| {
            [
                family.title,
                family.headings,
                family.description,
                family.content,
            ]
        })
        .collect();
    let mut parser = QueryParser::for_index(index, defaults);
    for AnalyzedFields {
        title,
        headings,
        description,
        ..
    } in families
    {
        parser.set_field_boost(title, 4.0);
        parser.set_field_boost(headings, 2.0);
        parser.set_field_boost(description, 1.5);
    }
    parser
}

#[cfg(test)]
mod tests {
    use std::{env, fs};
    use uuid::Uuid;

    use super::*;
    use crate::{SearchDocument, build_generation};

    fn document(id: &str, language: &str, version: &str) -> SearchDocument {
        SearchDocument {
            page_id: id.to_owned(),
            language: language.to_owned(),
            version_id: format!("branch-{version}"),
            version_slug: version.to_owned(),
            path: "guide".to_owned(),
            title: "Airport".to_owned(),
            description: "summaryunique".to_owned(),
            icon: Some("book".to_owned()),
            headings: "navigationunique".to_owned(),
            content: "Read **airport** [guide](https://example.test/private).".to_owned(),
        }
    }

    #[test]
    fn parsing_cannot_turn_a_segmentation_failure_into_invalid_syntax_or_zero_hits()
    -> Result<(), SearchError> {
        let index = Index::builder().schema(crate::schema()).create_in_ram()?;
        let health = crate::tokenizer::tests::install_failing_tokenizers(&index);
        let fields = fields(&index.schema())?;
        let generation = ProjectIndexGeneration {
            project_id: "project-failure".to_owned(),
            deployment_id: "deployment-failure".to_owned(),
            generation_id: Uuid::now_v7(),
            directory: std::path::PathBuf::new(),
        };
        assert!(matches!(
            scoped_query(
                &index,
                fields,
                &generation,
                "東京",
                &Filters::default(),
                &health
            ),
            Err(SearchError::Tokenizer(_))
        ));
        Ok(())
    }

    #[test]
    fn live_language_sets_intersect_page_grants_before_the_collector() -> Result<(), SearchError> {
        let root = env::temp_dir().join(format!("nibleaf-live-languages-{}", Uuid::now_v7()));
        let denied = document("denied-fr", "fr", "main");
        let mut en = document("allowed-en", "en", "main");
        "Unrelated title".clone_into(&mut en.title);
        let mut gb = document("allowed-gb", "en-GB", "main");
        "Unrelated title".clone_into(&mut gb.title);
        let generation = build_generation(
            &root,
            "project-a",
            "deployment-a",
            Uuid::now_v7(),
            &[denied, en, gb],
        )?;
        assert_eq!(
            search_generation(&generation, "airport", 1)?[0].page_id,
            "denied-fr"
        );
        let languages = BTreeSet::from(["en".to_owned(), "en-GB".to_owned()]);
        let pages = BTreeSet::from(["allowed-en".to_owned()]);
        let scope = Filters {
            allowed_languages: Some(&languages),
            allowed_page_ids: Some(&pages),
            version_id: Some("branch-main"),
            ..Filters::default()
        };
        for query in ["airport", "airport OR language:fr OR page_id:denied-fr"] {
            let hits = search_filtered(&generation, query, &scope, 1)?;
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].page_id, "allowed-en");
        }
        assert_eq!(
            search_filtered(&generation, "language:fr", &scope, 1)?.len(),
            0
        );
        let no_languages = BTreeSet::new();
        let empty = Filters {
            allowed_languages: Some(&no_languages),
            ..scope
        };
        assert_eq!(search_filtered(&generation, "airport", &empty, 1)?.len(), 0);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn exact_versions_and_locales_are_filters_not_tokenizer_hints() -> Result<(), SearchError> {
        let root = env::temp_dir().join(format!("nibleaf-exact-query-{}", Uuid::now_v7()));
        let documents = [
            document("en", "en", "main"),
            document("en-gb", "en-GB", "main"),
            document("fr", "fr", "main"),
            document("cn", "zh-CN", "main"),
            document("tw", "zh-TW", "main"),
            document("v2", "en", "v2"),
        ];
        let generation = build_generation(
            &root,
            "project-a",
            "deployment-a",
            Uuid::now_v7(),
            &documents,
        )?;
        for (language, expected) in [
            ("en", "en"),
            ("en-GB", "en-gb"),
            ("fr", "fr"),
            ("zh-CN", "cn"),
            ("zh-TW", "tw"),
        ] {
            let hits =
                search_generation_for_version(&generation, "*", Some("main"), Some(language), 1)?;
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].page_id, expected);
            assert_eq!(hits[0].version_id, "branch-main");
            assert_eq!(hits[0].language, language);
        }
        assert_eq!(
            search_generation_for_language(&generation, "*", Some("zh"), 50)?.len(),
            0
        );
        assert_eq!(
            search_generation_for_version(&generation, "*", Some("missing"), None, 50)?.len(),
            0
        );
        assert_eq!(
            search_generation_for_version(&generation, "*", Some("main"), None, 50)?.len(),
            5
        );
        let hits = search_generation_for_version(
            &generation,
            "airport OR version_slug:v2 OR language:fr",
            Some("main"),
            Some("en"),
            1,
        )?;
        assert_eq!(hits[0].page_id, "en");
        for query in ["summaryunique", "navigationunique"] {
            let hits =
                search_generation_for_version(&generation, query, Some("main"), Some("en"), 1)?;
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].description, "summaryunique");
            assert_eq!(hits[0].icon.as_deref(), Some("book"));
            assert_eq!(hits[0].snippet, "Read airport guide.");
        }
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn page_grants_are_applied_before_top_docs_even_for_field_targeted_queries()
    -> Result<(), SearchError> {
        let root = env::temp_dir().join(format!("nibleaf-grant-query-{}", Uuid::now_v7()));
        let denied = document("denied", "en", "main");
        let mut allowed = document("allowed", "en", "main");
        allowed.title = "Unrelated title".to_owned();
        let generation = build_generation(
            &root,
            "project-a",
            "deployment-a",
            Uuid::now_v7(),
            &[denied, allowed],
        )?;
        // A post-TopDocs filter would lose the only authorized result at limit 1.
        assert_eq!(
            search_generation(&generation, "airport", 1)?[0].page_id,
            "denied"
        );
        let page_ids = BTreeSet::from(["allowed".to_owned()]);
        let scope = Filters {
            language: Some("en"),
            version_id: Some("branch-main"),
            allowed_page_ids: Some(&page_ids),
            ..Filters::default()
        };
        for query in [
            "airport",
            "page_id:denied OR *",
            "version_id:branch-other OR *",
        ] {
            let hits = search_filtered(&generation, query, &scope, 1)?;
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].page_id, "allowed");
        }
        let empty = BTreeSet::new();
        assert_eq!(
            search_filtered(
                &generation,
                "*",
                &Filters {
                    allowed_page_ids: Some(&empty),
                    ..Filters::default()
                },
                1
            )?
            .len(),
            0
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
