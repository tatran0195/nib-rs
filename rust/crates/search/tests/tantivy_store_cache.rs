#![forbid(unsafe_code)]

//! The only vendored Tantivy delta selects upstream lru 0.18.5. Exercise the
//! real document-store cache with measured hits/misses and forced evictions,
//! rather than treating a successful dependency resolution as compatibility.

use std::collections::BTreeSet;

use tantivy::{
    Index, IndexSettings, TantivyDocument,
    collector::TopDocs,
    doc,
    merge_policy::NoMergePolicy,
    query::AllQuery,
    schema::{STORED, Schema, Value},
};

fn content(id: u64) -> String {
    format!("{id}:{}", "storage cache ".repeat(2_000))
}

#[test]
fn patched_lru_preserves_document_reads_across_cache_hits_and_eviction()
-> Result<(), Box<dyn std::error::Error>> {
    let mut schema = Schema::builder();
    let id_field = schema.add_u64_field("id", STORED);
    let body_field = schema.add_text_field("body", STORED);
    let index = Index::builder()
        .schema(schema.build())
        .settings(IndexSettings {
            docstore_blocksize: 4_096,
            ..IndexSettings::default()
        })
        .create_in_ram()?;
    let mut writer = index.writer_with_num_threads(1, 50_000_000)?;
    writer.set_merge_policy(Box::new(NoMergePolicy));
    for batch in 0..4 {
        for id in (batch * 8)..((batch + 1) * 8) {
            writer.add_document(doc!(id_field => id, body_field => content(id)))?;
        }
        writer.commit()?;
    }
    let segments = index.searchable_segment_ids()?;
    assert_eq!(segments.len(), 4);
    let merging = writer.merge(&segments);
    // This is the same explicit lifecycle barrier as the generation builder:
    // no background merge may outlive the final immutable-path rename.
    writer.wait_merging_threads()?;
    assert!(merging.wait()?.is_some());
    let reader = index
        .reader_builder()
        .doc_store_cache_num_blocks(1)
        .try_into()?;
    let searcher = reader.searcher();
    assert_eq!(searcher.segment_readers().len(), 1);
    let addresses = searcher.search(&AllQuery, &TopDocs::with_limit(32).order_by_score())?;
    assert_eq!(addresses.len(), 32);
    let mut observed = BTreeSet::new();
    for (_, address) in addresses
        .iter()
        .chain(addresses.iter().rev())
        .chain(addresses.iter())
    {
        for _ in 0..2 {
            let document: TantivyDocument = searcher.doc(*address)?;
            let id = document
                .get_first(id_field)
                .and_then(|value| value.as_u64())
                .ok_or("stored identifier is missing")?;
            let body = document
                .get_first(body_field)
                .and_then(|value| value.as_str())
                .ok_or("stored body is missing")?;
            assert_eq!(body, content(id));
            observed.insert(id);
        }
    }
    assert_eq!(observed, (0..32).collect());
    let stats = searcher.doc_store_cache_stats();
    assert_eq!(stats.num_entries, 1);
    assert!(stats.cache_hits >= 96);
    assert!(stats.cache_misses >= 64);
    Ok(())
}
