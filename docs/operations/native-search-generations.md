# Native Tantivy generation operations

> **Current staged-search delta — ADR-0017 (2026-10-16):** the distinct opt-in native public transport, in-budget native/transition read-only identity, selected UTF-16/literal contract, scoped BM25/stable pre-limit ties, bounded owned JSON, aggregate-only metrics, offline rebuild and local load/network/HTTP proofs are implemented. It is **disabled by default**, with no production routing, UI/caller selection or legacy removal. Earlier statements below that transport/scoring are unimplemented describe the prior milestone and are superseded by the [current candidate contract and release boundary](../operations/public-search-candidate.md). Selected 200-UTF-16 native validation differs from installed Zod 4.5.4 scalar counting; no exact Unicode parity claim.

**Status:** publication is connected to durable native-generation registration/build/promotion and will not become `READY` before promotion. An authenticated project-member management query route exists; there is still **no native public reader/site query route or reader cutover**, so this is not a claim that public Nibleaf search traffic uses Tantivy yet.

## Scope and invariants

The Rust `nibleaf-search` crate provides lexical-only, physical indexes under a configured persistent root:

```text
${NIBLEAF_SEARCH_INDEX_ROOT}/projects/<sha256-project-id>/generations/<generation-uuid>/
```

No raw project ID is a filesystem component. A final generation contains Tantivy files plus `manifest.json`. The manifest is written last in a same-directory staging build, then the staging directory is renamed to its final generation path only after validation.

The manifest contains:

- required manifest format **3**, exact runtime identity, generation, project, and deployment identities;
- an explicit searchable document count;
- a page-ID-to-source-checksum map, which contains no page content; and
- an aggregate SHA-256 checksum over the ordered map.

Each Tantivy stored document includes exact version/locale, routing/result fields, authored body and its source checksum. Validation before promotion requires the closed source-format-2 schema and format-3 manifest with exact engine/analyzer/patch identity, recomputes each checksum from those stored source fields, and checks manifest identity/count/aggregate, every project/deployment binding, and the complete live-document map. It is an integrity/completeness check for trusted persistent storage, **not** a cryptographic defense against a host administrator who can alter both PostgreSQL and the mounted index volume.

## Registry lifecycle

Migration `rust/migrations/202610160012_search_generation_registry.sql` adds:

- `rust_search_generation`, keyed by a generation UUID and constrained to one `(project_id, deployment_id)` build;
- a project/deployment and project/tenant FK chain, so an index row cannot bind a foreign deployment; and
- `rust_search_active_generation`, used by member management/build state, **not** the public publication selector. [ADR-0015](../architecture/decisions/0015-published-search-ceilings-and-ready-selection.md) selects the highest READY deployment and its exact registry generation.

Migration `202610160013_search_empty_generations.sql` permits an intentionally empty, but fully validated, generation. This preserves availability for a project with navigation groups but no searchable `PAGE` records.

A publication transaction appends this tenant-scoped `search.generation.requested` v1 payload **before** its `publication.requested` completion event, using the same canonical, type-tagged JSON SHA-256 identity as the registry:

```json
{
  "project_id": "opaque project ID",
  "deployment_id": "opaque deployment ID",
  "snapshot_sha256": "lowercase 64-character SHA-256"
}
```

The worker rechecks the tenant/project/deployment binding under a short registration transaction and idempotently assigns/reuses the registry generation UUID. It then:

1. leases `pending` work, or reclaims an expired `building` lease with a **new** PostgreSQL token;
2. verifies the canonical snapshot checksum after PostgreSQL JSONB key normalization;
3. parses the typed format-2 version/language/visibility contract, extracts only enabled-language visible/indexable `PAGE` records (not groups, hidden/noindex or disabled locales), builds outside a database transaction, and heartbeats the registry lease while joining the blocking Tantivy task;
4. reuses an existing final generation only if manifest validation proves it matches the snapshot-derived document digest; and
5. in one PostgreSQL transaction, marks it `ready`, swaps the active pointer, and marks the former active generation `superseded`.

`publication.requested` leaves the deployment `BUILDING` and is retried while the registry row is absent, pending, or building. It emits `publication.ready` only once the row is `ready`; a terminal generation state makes the deployment `FAILED` and its event terminal.

An old worker whose token was reclaimed may finish its local filesystem work but cannot promote or modify the newer claim. A failed build becomes `pending` for a retryable failure or `failed` for a permanent input/snapshot/integrity failure. Superseded index directories are retained; garbage collection is deliberately not implemented yet.

## Format and query compatibility

Follow the [source-format migration](../migration/native-search-format2.md) and [runtime upgrade](../migration/native-search-runtime-upgrade.md). Current publication/source format is **2**; the generation manifest is **3**, with mandatory exact compiled Tantivy/index-format identity, Lindera identity, dependency-patch revision and analyzer revision. Snapshot 1 fails permanently; manifest-1/2 generations and mismatched/missing runtime evidence are unavailable, never reinterpreted. Do not edit old manifests, infer omitted visibility or mutate old publication snapshots. Coordinate API/worker rollout and create a new consciously reviewed publication; a search-only rebuild/backfill command is not implemented.

Member management supports optional exact `version` slug and `language` code. Omitted scopes search all eligible versions/locales; unknown supplied scopes yield no hits. Results add exact version identity, description/icon and bounded plain-text snippets. This is not public default/fallback behavior. Query-parameter rejection uses Problem Details and rejects client page-scope input.

The public-admission primitive itself remains read-only and performs no I/O outside PostgreSQL. A separate non-HTTP query helper enforces its project binding, exact version/locale and audience page terms before result collection. No public route, public cache/analytics policy or optional-session transport has been installed.

## Non-HTTP published selection and controls

[ADR-0015](../architecture/decisions/0015-published-search-ceilings-and-ready-selection.md) and the [policy guide](../migration/native-public-search-scope.md) now define a verified composition: fresh admission, highest-READY selection, registry evidence and live controls share one short REPEATABLE READ / READ ONLY view. This is a read operation, not a change to publication capture isolation/locks. The transaction commits before returning an opaque one-shot operation; consuming execution verifies snapshot/registry/runtime/index evidence synchronously. Preparation starts no background task and receives no filesystem root.

The user-selected policy bounds live changes by published eligibility/defaults/cap. Exact published-enabled codes intersect current same-project enabled rows; request/published/live caps intersect before collection. Missing/live-disabled default fallback is empty, not forced serving or another language. Malformed known caps fail closed. Live relaxations within the published ceiling remain possible; widening it needs a new consciously reviewed publication.

**Superseded does not mean unused:** promotion precedes READY and may leave the active pointer on a newer incomplete or failed deployment. The previous highest READY publication still needs its retained generation. Public selection allows only that deployment’s matching ready/superseded generation and never skips a broken latest READY to an older one. Future GC must protect highest READY and in-flight readers as well as build/recovery evidence; neither generation state nor active-pointer membership is deletion authority. No applied migration comment is rewritten and no GC command is added.

Public HTTP transport and its optional actor/session/cache/error/analytics/schema/privacy contracts remain absent. [ADR-0016](../architecture/decisions/0016-supervised-search-query-lifecycle.md) now supplies the API-driven service/driver for bounded admission, preparation/transfer/validation ceilings, tracked SQL and joined execution/drain. The member route uses it; published queries have a non-HTTP facade. See [query runtime configuration, metrics and limits](search-query-runtime.md). It does not guarantee hard CPU/network/filesystem preemption, a hard drain deadline or aggregate RSS. Current full-snapshot hashing/full-generation scanning is O(publication size); no largest-project load or throughput guarantee has been verified. Do not call synchronous execution directly on an async worker or cache the prepared admission object.

## Configuration and storage

**The volume contains readable tenant page bodies and metadata**, including retained superseded/staging data. Apply restrictive filesystem/backup permissions and tenant retention rules. Relational project deletion does not erase these files; native build/promotion fencing and receipted purge/GC/recovery remain ADR-0012/0013 work, so sole-tenant erasure remains blocked.

`NIBLEAF_SEARCH_INDEX_ROOT` must name a persistent, shared filesystem volume available to every worker that builds native search and every API replica that serves the authenticated management query. Because publication now emits native-search work, it is required in every environment that processes publication outbox events. A missing root registers then terminally fails the generation instead of writing to ephemeral container storage; the linked publication will ultimately be marked `FAILED` rather than falsely becoming ready.

Both API and worker also reject startup unless the registry tables/pointer and the `202610160013` zero-document constraint are present. This fail-closed check prevents admitting a navigation-only publication into a pre-expansion database that could only fail it after HTTP acceptance.

Before enabling native publication processing in an environment, operators must verify all of the following:

1. API and worker replicas mount the same durable path with the `nibleaf` runtime user's required read/write permissions.
2. Capacity includes current active generations, retained superseded generations, staging space, and Tantivy merge overhead; disk-full alerts are configured.
3. The volume backup/restore procedure includes index data **and** PostgreSQL. Restoring only one is recoverable by a controlled rebuild, but must not silently point readers to a mismatched generation.
4. `manifest.json` validation and a corrupt/missing-generation recovery drill have been executed against a non-production project.
5. The worker can renew the five-minute build lease at its one-minute heartbeat under expected largest-project load, and those timing values have been reviewed together.

## Recovery boundaries

- A missing/corrupt final generation is project-local unavailable after successful management admission (`503`); it must never fall back to another project, legacy Orama, or Qdrant. A future public route must first conceal denied/taken-down/unpublished sites as `404`, then report an admitted generation failure as unavailable.
- A stale `.UUID.building` directory is unpublishable. Only the worker that successfully owns/reclaims that generation registry lease may clear it.
- Do not manually edit a final generation or `manifest.json`. Create a reviewed rebuild workflow instead; registry history and retained files are evidence for diagnosis.
- This slice does not yet delete Orama/Qdrant data, remove their configuration, or provide a native **public reader/site** query endpoint. The read-only Rust reader-admission primitive deliberately opens no generation; its separate exact-scope query helper is non-HTTP and remaining public compatibility requirements are documented in `docs/architecture/native-public-search-compatibility.md`. Those actions remain governed by ADR-0013's separately reviewed compatibility/cutover phase.

## Dependency release gate

The four previously active search warnings are resolved under [ADR-0014](../architecture/decisions/0014-search-runtime-dependency-remediation.md). Current code uses Tantivy 0.26.2 release source with exactly one normalized-manifest dependency edit to upstream LRU 0.18.5, plus direct Lindera 6.2.0 and its two pinned embedded dictionaries. The wrapper/analysis/YAML graph and old `encoding` dependency are removed. The native adapter latches segmentation errors across tokenizer clones; builders explicitly join indexing/merge work and check after commit and before manifest creation/rename, and queries check after parsing and before search.

Required checks from `rust/`:

```bash
python3 scripts/check-search-dependencies.py --self-test
bash scripts/check-inactive-rsa.sh
cargo audit --ignore RUSTSEC-2023-0071 --deny warnings
cargo +1.88.0 check --locked --workspace --all-targets
```

The guard checks every vendored upstream file, the sole allowed manifest delta, exact runtime/dictionary pins and declared MSRVs. The real Rust 1.88 compiler check also passed locally; resolver 3 selects compatible dependency versions. Refreshed raw audit still fails **only** for inactive RSA, while the existing RSA-only exception now passes **without warnings**, with warnings denied in CI. No search advisory ignore was added. Keep the patch reviewable and replace it with a compatible official upstream release; the current graph's mitigation is not blanket security or public-cutover approval.

## Verification

The disposable PostgreSQL harness runs `postgres_generation_registry`, `postgres_public_reader_admission`, and `postgres_publication_search_ordering` after applying the real legacy and Rust migration chains. It was executed successfully on local non-Docker PostgreSQL 17.11: the registry proof exercised incompatible-runtime promotion refusal without an active pointer, stale-worker promotion denial, distinct reclaim tokens, final-generation reuse after a crash before promotion, terminal-state evidence, active-pointer replacement with the prior generation retained as `superseded`, and promotion of a valid zero-document navigation-only generation; the admission proof exercised all three access modes, live takedown concealment, page/site/zero audience grants, revoked sessions, workspace override, project-bound cookies, and its read-only activity invariant; the publication proof covered `BUILDING` retry/no early ready, ready-after-promotion, and terminal search-failure propagation. The registry proof also promotes a mixed version/visibility corpus and records terminal refusal of format-1 input. Real reader grants are exercised against the non-HTTP query helper with exact locale/version and zero scopes, including an admission/generation project mismatch. The worker/HTTP portion drives native starter plus ordinary format-2 publication; `test-search-publication-scope.sh` rejects foreign locale/branch references before acceptance, mutates later drafts/locales/branch names, and verifies immutable description/icon/snippet/headings and exact version/locale results. The second test in `postgres_public_reader_admission` now exercises the full non-HTTP composition: blocked widening/live tightening, frozen defaults after draft changes, promotion-before-READY and failed-newer selection, absent/failed/corrupt evidence, composed workspace/reader scope and unchanged activity. A real PostgreSQL lock interleaving proves live restrictions and selection cannot mix MVCC views. The harness also asserts no public route is installed. The third admission-test entry exercises the real service: snapshot/manifest/document/source limits, live controls projection, retained capacity after a lock-blocked caller drop, server-timeout/drain, pool-wait deadline, LOCAL non-leakage and bounded reader grants/full-site behavior. The HTTP helper rejects all six zero-valued settings and proves tiny-limit 503, graceful drain and DEBUG trace/metric sentinel privacy. Consult the verification report for the current **109 Cargo cases / seven separately non-skip DB functions**; load/hard-failure/recovery gates remain open.
