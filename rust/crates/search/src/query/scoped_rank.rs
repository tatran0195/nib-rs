//! Public BM25 sees only the exact authorized corpus. No global document/token
//! totals or document frequencies are substituted when scope is empty/missing.
use super::Filters;
use crate::{
    SearchError, SearchFields, TokenizerHealth,
    execution::budget::{QueryBudget, checkpoint},
    stored_document,
};
use std::{
    cmp::Reverse,
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tantivy::{
    DocAddress, DocId, DocSet, Index, Searcher, SegmentReader, TantivyDocument, Term,
    collector::TopDocs,
    query::{Bm25StatisticsProvider, Query},
    schema::{Field, IndexRecordOption},
};

type StableKey = (Arc<str>, Arc<str>);
type SegmentKeys = Arc<BTreeMap<DocId, StableKey>>;

struct ScopedStatistics<'a> {
    searcher: &'a Searcher,
    documents: u64,
    token_counts: BTreeMap<Field, u64>,
    keys: Vec<SegmentKeys>,
    budget: Option<&'a QueryBudget>,
}

impl ScopedStatistics<'_> {
    fn interrupt(&self) -> tantivy::Result<()> {
        checkpoint(self.budget)
            .map_err(|_| tantivy::TantivyError::InvalidArgument("search interrupted".to_owned()))
    }
}

impl Bm25StatisticsProvider for ScopedStatistics<'_> {
    fn total_num_docs(&self) -> tantivy::Result<u64> {
        self.interrupt()?;
        Ok(self.documents)
    }
    fn total_num_tokens(&self, field: Field) -> tantivy::Result<u64> {
        self.interrupt()?;
        Ok(self
            .token_counts
            .get(&field)
            .copied()
            .unwrap_or(self.documents))
    }
    fn doc_freq(&self, term: &Term) -> tantivy::Result<u64> {
        self.interrupt()?;
        let mut count = 0;
        for (segment, keys) in self.searcher.segment_readers().iter().zip(&self.keys) {
            if keys.is_empty() {
                continue;
            }
            let inverted = segment.inverted_index(term.field())?;
            let Some(mut postings) = inverted.read_postings(term, IndexRecordOption::Basic)? else {
                continue;
            };
            for (position, doc_id) in keys.keys().enumerate() {
                if position.is_multiple_of(64) {
                    self.interrupt()?;
                }
                if postings.doc() < *doc_id {
                    postings.seek(*doc_id);
                }
                if postings.doc() == *doc_id {
                    count += 1;
                }
            }
        }
        Ok(count)
    }
}

/// Exact token totals are recomputed with the compiled index analyzers over
/// authorized stored source only, matching indexing inputs (including MDX).
/// Per-document BM25 length normalization remains Tantivy's quantized norm.
fn statistics<'a>(
    index: &Index,
    searcher: &'a Searcher,
    fields: SearchFields,
    filters: &Filters<'_>,
    health: &TokenizerHealth,
    budget: Option<&'a QueryBudget>,
) -> Result<ScopedStatistics<'a>, SearchError> {
    let mut analyzers = BTreeMap::new();
    for family in [fields.general, fields.japanese, fields.chinese] {
        for field in [
            family.title,
            family.headings,
            family.description,
            family.content,
        ] {
            analyzers.insert(field, index.tokenizer_for_field(field)?);
        }
    }
    let mut stats = ScopedStatistics {
        searcher,
        documents: 0,
        token_counts: BTreeMap::new(),
        keys: Vec::new(),
        budget,
    };
    for field in analyzers.keys() {
        stats.token_counts.insert(*field, 0);
    }
    let mut slots = 0_u64;
    for (ordinal, segment) in searcher.segment_readers().iter().enumerate() {
        checkpoint(budget)?;
        slots += u64::from(segment.max_doc());
        if budget.is_some_and(|b| slots > b.config.max_documents as u64) {
            return Err(SearchError::ResourceLimit("index document slots"));
        }
        let ordinal =
            u32::try_from(ordinal).map_err(|_| SearchError::Integrity("segment ordinal"))?;
        let mut keys = BTreeMap::new();
        for doc_id in 0..segment.max_doc() {
            checkpoint(budget)?;
            if segment.is_deleted(doc_id) {
                continue;
            }
            let document: TantivyDocument = searcher.doc(DocAddress::new(ordinal, doc_id))?;
            let source = stored_document(&document, fields)?;
            if !filters.permits(&source) {
                continue;
            }
            stats.documents += 1;
            keys.insert(
                doc_id,
                (
                    Arc::from(source.title.as_str()),
                    Arc::from(source.page_id.as_str()),
                ),
            );
            let family = fields.analyzed(&source.language);
            for (field, text) in [
                (family.title, &source.title),
                (family.headings, &source.headings),
                (family.description, &source.description),
                (family.content, &source.content),
            ] {
                let analyzer = analyzers
                    .get_mut(&field)
                    .ok_or(SearchError::Integrity("scoped analyzer missing"))?;
                let mut stream = analyzer.token_stream(text);
                let mut count = 0_u64;
                while stream.advance() {
                    count += 1;
                    if count.is_multiple_of(64) {
                        checkpoint(budget)?;
                    }
                }
                *stats.token_counts.entry(field).or_default() += count;
            }
            health.check()?;
        }
        stats.keys.push(Arc::new(keys));
    }
    checkpoint(budget)?;
    Ok(stats)
}

#[derive(Clone, Copy)]
pub(super) struct RankingOptions<'a> {
    pub(super) health: &'a TokenizerHealth,
    pub(super) budget: Option<&'a QueryBudget>,
}

pub(super) fn search(
    index: &Index,
    searcher: &Searcher,
    fields: SearchFields,
    filters: &Filters<'_>,
    query: &dyn Query,
    options: RankingOptions<'_>,
    limit: usize,
) -> Result<Vec<(f32, DocAddress)>, SearchError> {
    let RankingOptions { health, budget } = options;
    let stats = statistics(index, searcher, fields, filters, health, budget)?;
    if stats.documents == 0 {
        return Ok(Vec::new());
    }
    let keys: Vec<_> = searcher
        .segment_readers()
        .iter()
        .zip(&stats.keys)
        .map(|(segment, keys)| (segment.segment_id(), Arc::clone(keys)))
        .collect();
    let missing = Arc::new(AtomicBool::new(false));
    let marker = Arc::clone(&missing);
    // Stable title/page ordering participates in TopDocs itself, not a sort
    // after a DocAddress-dependent truncation. Denied docs cannot move ties.
    let collector = TopDocs::with_limit(limit).tweak_score(move |segment: &SegmentReader| {
        let map = keys
            .iter()
            .find(|(id, _)| *id == segment.segment_id())
            .map(|(_, map)| Arc::clone(map));
        let marker = Arc::clone(&marker);
        move |doc_id, score| {
            let key = map.as_ref().and_then(|map| map.get(&doc_id)).cloned();
            if key.is_none() || !score.is_finite() {
                marker.store(true, Ordering::Relaxed);
            }
            (score, Reverse(key))
        }
    });
    let result = searcher.search_with_statistics_provider(query, &collector, &stats);
    checkpoint(budget)?;
    if missing.load(Ordering::Relaxed) {
        return Err(SearchError::Integrity(
            "collector escaped authorized corpus",
        ));
    }
    let result = result?;
    if result.iter().any(|((score, _), _)| !score.is_finite()) {
        return Err(SearchError::Integrity("non-finite scoped score"));
    }
    Ok(result
        .into_iter()
        .map(|((score, _), address)| (score, address))
        .collect())
}
