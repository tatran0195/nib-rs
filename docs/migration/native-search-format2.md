# Native publication/search format 2 migration

**Status:** implemented and verified in the disposable local environment; production rollout is not executed or approved by this document.  
**Date:** 2026-10-16  
**Related:** ADR-0013, [public-search compatibility](../architecture/native-public-search-compatibility.md), [generation operations](../operations/native-search-generations.md).

> **Current-runtime follow-on:** snapshot/source format 2 remains the current native publication contract. Newly built generations now require **manifest 3** with exact Tantivy/Lindera/patch identity. Manifest 2 is no longer accepted by current binaries; also follow the [runtime upgrade](native-search-runtime-upgrade.md). This document records the earlier source/visibility expansion, not permission to relabel an old index.

## Why this is a format change

Native snapshot format 1 captured only default-branch/enabled-language minimal page data. Native index/manifest format 1 could not establish immutable version identity, hidden/noindex eligibility, or public rendering fields. Treating missing facts as public-safe would be an authorization/product regression.

Format 2 deliberately requires new immutable evidence:

- A publication stores every branch as a version (`id`, `name`, collision-safe `slug`, `is_default`), every language with its enabled/default state, and every page's exact version/language binding, hidden flag, SEO config, description, icon, sidebar config, content, and revision. Capture rejects cross-project language/branch references before deployment/outbox acceptance.
- Project input and all language/version/page collections are read in one PostgreSQL statement after taking the project share lock. Variable substitution is frozen into title, description, and body with a one-million-byte bound per expanded text field.
- Only enabled-language, visible, indexable `PAGE` records enter the index. `GROUP`, hidden, noindex, and disabled-language records remain snapshot facts but never search hits. A wholly ineligible corpus is a valid zero-document generation.
- The format-2 document digest includes exact version ID/slug, language, path/title, description, icon presence/value, headings, and authored content. Validation recomputes the checksum from stored source fields and requires the exact index schema; it does not merely trust a copied checksum field.
- Version, exact locale, and admitted reader page grants are mandatory query intersections before `TopDocs`. Admission is project-bound. A new request must derive a fresh admission; these objects are not reusable authorization tickets.

## Explicit compatibility consequences

| Existing artifact/consumer | New behavior | Required action |
|---|---|---|
| Ready native manifest/index format 1 | New binaries refuse it; an admitted management query returns `503`, not guessed results. | Plan a management-query maintenance window and create a reviewed format-2 publication. Do not relabel or overwrite a retained generation. |
| Pending native snapshot format 1 | Worker extraction is a permanent snapshot failure, persisted with failed-state evidence. It cannot be promoted by format-2 code. | Drain existing format-1 work before rollout, or review terminal handling and a new publication explicitly. Do not silently reinterpret the old snapshot. |
| Legacy TypeScript `SiteSnapshot` | It is not native snapshot format 2 and is not accepted as build input by inference. | Preserve legacy public authority until the separately reviewed publication/site adapter and search cutover exist. This migration is not that adapter. |
| Native management `language` filter | It now means an exact stored code (`en` is not `en-GB` or `fr`; `zh-CN` is not `zh-TW`). Unknown values produce no hits. | Update any management callers that previously depended on tokenizer-family behavior. Public default/fallback semantics are still a separate contract. |
| Native management query/response | Optional exact `version` slug; omitted version searches all eligible versions. Hits add `version_id`, `version_slug`, description, optional icon, and bounded plain-text snippet. | Use returned version metadata to disambiguate same-path results. The management envelope remains distinct from the legacy public hit envelope. |
| Previously indexed hidden/noindex pages | They are absent from the new corpus, including for project members. | Treat this as published-site search, not a complete draft/editor search. Review hidden/noindex state before republishing. |
| Markdown/MDX rendering | Body lexical indexing retains authored text. Snippets use a deterministic CommonMark projection, not an MDX renderer or trusted HTML, and are bounded to 240 Unicode scalars plus up to two ellipses. | Escape all result text in clients. Characterize MDX/Arabic/multilingual relevance and snippet differences before public cutover. |

The generation directory layout, PostgreSQL registry tables, nonnegative document-count constraint, and `search.generation.requested` **event** schema version 1 are unchanged. Snapshot/source format 2 or current manifest 3 does not mean event schema 2. No additional DDL is necessary for this format expansion beyond the existing required Rust migrations through `202610160013`.

## Reviewed rollout order

1. Inventory the environment's native deployments and on-disk manifest formats, confirm a shared durable volume, and back up PostgreSQL plus index files. No production inventory or backup/recovery drill has been executed here.
2. Identify every management caller and every project still consumed by legacy site rendering. Native format-2 snapshots are not a drop-in legacy renderer input. Do not co-route native publication into a legacy public delivery project without its approved adapter/publication-parity plan.
3. Quiesce native publication admission and drain outstanding old-format work. Coordinate API and worker binary rollout; a mixed old/new worker set must not consume incompatible snapshot/index formats. This document does not provide an automatic drain/rebuild/rollback operator command.
4. In a staging project, create a **new**, reviewed publication from the intended draft state. Existing published content may differ from current drafts; republishing must be a conscious content decision, not a search-only conversion.
5. Verify new snapshot `schema_version=2`, current manifest `format_version=3` and exact runtime identity, eligible document count, exact versions/locales, live reader scope, and registry promotion before accepting the management result contract. New generation UUIDs and publication versions preserve the historical artifacts.
6. Retain old deployments/indexes as evidence. Do not edit manifests, rewrite immutable snapshots, delete original content, or change registry pointers manually. Automated safe rebuild/GC and recovery drills remain operations work.
7. Native public HTTP search remains absent. The [published-scope follow-on](native-public-search-scope.md) now supplies non-HTTP immutable defaults, published ceilings/live restrictions and highest-READY selection. Its distinct route, optional native/transition actor transport, bounded complete execution/shutdown, public request/result/cache/error/analytics policy and relevance/privacy/client migration must still pass the public compatibility gate before removing Orama/Qdrant. No native fallback/shadow runtime is introduced.

Read-only inventory template (not a production execution claim):

```sql
SELECT status::text,
       COALESCE(snapshot->>'schema_version', 'legacy/unknown') AS snapshot_format,
       COUNT(*)
FROM deployment
GROUP BY 1, 2 ORDER BY 1, 2;

SELECT state, COUNT(*) FROM rust_search_generation GROUP BY state ORDER BY state;
```

## Executed verification

The local non-Docker PostgreSQL 17.11 harness proves:

- malformed format-2 and unsupported format-1 snapshots persist terminal registry failure evidence;
- a mixed version/visibility/language corpus produces only eligible format-2 documents and promotes with immutable metadata; navigation-only zero-document promotion still works;
- real reader-session/audience grants are applied before the result limit, zero grants yield no hits, exact locale/version filters remain effective for site-wide readers, and an admission from another project is rejected;
- native HTTP capture rejects foreign-project language **and** branch references with no deployment/search intent accepted;
- later draft/locale/branch edits cannot change a captured generation; exact member HTTP queries still return the frozen metadata, description/headings matches, and bounded snippets;
- client page-scope input is rejected and no public HTTP search route is installed.

The verification report records the final executed source/database gates. None of these tests is an Orama/Qdrant production traffic migration or a claim of full public relevance parity.
