use super::*;
use crate::{SearchDocument, build_generation};
use std::{env, fs};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;
fn page(id: &str, language: &str, version: &str, text: &str) -> SearchDocument {
    SearchDocument {
        page_id: id.to_owned(),
        language: language.to_owned(),
        version_id: version.to_owned(),
        version_slug: version.to_owned(),
        title: "Identical title".to_owned(),
        path: format!("/{id}"),
        description: String::new(),
        icon: None,
        headings: String::new(),
        content: text.to_owned(),
    }
}
fn public_hits(
    generation: &ProjectIndexGeneration,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, SearchError> {
    let pages = BTreeSet::from(["allowed-a".to_owned(), "allowed-b".to_owned()]);
    let languages = BTreeSet::from(["en".to_owned()]);
    search_filtered_with_budget(
        generation,
        query,
        &Filters {
            version_id: Some("v1"),
            allowed_languages: Some(&languages),
            allowed_page_ids: Some(&pages),
            public_literals: true,
            ..Filters::default()
        },
        limit,
        None,
    )
}
#[test]
fn excluded_pages_versions_languages_cannot_change_public_scores_or_pre_limit_ties() -> TestResult {
    let root = env::temp_dir().join(format!("nibleaf-scoped-ranking-{}", Uuid::now_v7()));
    let mut baseline = vec![
        page("allowed-b", "en", "v1", "scopeword second"),
        page("allowed-a", "en", "v1", "scopeword second"),
    ];
    let first = build_generation(
        &root,
        "ranking-project",
        "ranking-publication",
        Uuid::now_v7(),
        &baseline,
    )?;
    // Change both document frequencies and average field lengths outside each
    // authority boundary, and perturb internal DocAddress/segment ordering.
    for index in 0..72 {
        let (lang, ver) = match index % 3 {
            0 => ("en", "v1"),
            1 => ("fr", "v1"),
            _ => ("en", "v2"),
        };
        let mut denied = page(
            &format!("denied-{index}"),
            lang,
            ver,
            &"scopeword second AND body_general ".repeat(index + 1),
        );
        denied.title = "scopeword ".repeat(index % 12 + 1).trim_end().to_owned();
        baseline.insert(0, denied);
    }
    let second = build_generation(
        &root,
        "ranking-project",
        "ranking-publication",
        Uuid::now_v7(),
        &baseline,
    )?;
    for query in [
        "scopeword",
        "scopeword second",
        "scopeword AND",
        "body_general:scopeword",
    ] {
        for limit in [1, 2, 50] {
            let left = public_hits(&first, query, limit)?;
            let right = public_hits(&second, query, limit)?;
            let projection = |hits: Vec<SearchHit>| {
                hits.into_iter()
                    .map(|hit| (hit.page_id, hit.score.to_bits()))
                    .collect::<Vec<_>>()
            };
            let left = projection(left);
            assert_ne!(left.len(), 0);
            assert_eq!(left[0].0, "allowed-a");
            assert_eq!(left, projection(right), "query/limit invariance failed");
        }
    }
    assert_eq!(public_hits(&second, "/.*/", 50)?.len(), 0);
    assert_eq!(public_hits(&second, "page_id:denied", 50)?.len(), 0);
    assert_eq!(public_hits(&second, "   ", 50)?.len(), 0);
    fs::remove_dir_all(root)?;
    Ok(())
}
#[test]
fn public_literal_queries_have_characterized_japanese_chinese_and_field_priority() -> TestResult {
    let root = env::temp_dir().join(format!("nibleaf-public-language-{}", Uuid::now_v7()));
    let mut title_match = page("title", "en", "v1", "ordinary content");
    title_match.title = "scopeword".to_owned();
    let docs = vec![
        title_match,
        page("body", "en", "v1", "scopeword"),
        page("japanese", "ja", "v1", "東京大学で日本語を学びます"),
        page("chinese", "zh-CN", "v1", "北京大学中文搜索"),
    ];
    let generation = build_generation(
        &root,
        "language-project",
        "language-publication",
        Uuid::now_v7(),
        &docs,
    )?;
    for (language, query, expected) in [
        ("ja", "日本語", "japanese"),
        ("zh-CN", "中文", "chinese"),
        ("en", "scopeword", "title"),
    ] {
        let languages = BTreeSet::from([language.to_owned()]);
        let hits = search_filtered_with_budget(
            &generation,
            query,
            &Filters {
                version_id: Some("v1"),
                allowed_languages: Some(&languages),
                language: Some(language),
                public_literals: true,
                ..Filters::default()
            },
            1,
            None,
        )?;
        assert_eq!(hits.first().map(|hit| hit.page_id.as_str()), Some(expected));
    }
    fs::remove_dir_all(root)?;
    Ok(())
}
