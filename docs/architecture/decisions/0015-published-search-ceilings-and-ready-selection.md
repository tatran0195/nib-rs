# ADR-0015: published search ceilings, live restrictions and READY selection

**Status:** user policy selected; implemented and verified locally as a non-HTTP library composition  
**Date:** 2026-10-16  
**Related:** [ADR-0013](0013-project-scoped-tantivy-lindera-search.md), [ADR-0014](0014-search-runtime-dependency-remediation.md), [compatibility matrix](../native-public-search-compatibility.md), [migration and policy guide](../../migration/native-public-search-scope.md)

## Verified context

Executable legacy code, rather than its comments alone, establishes:

- `apps/server/src/actions/sites.ts::getPublished` chooses the highest-version `READY` deployment. A newer incomplete deployment does not replace it.
- The same action calls `overlayLiveChrome`: project search configuration and the enabled flags of matching published languages are overlaid from live rows. The published language list, default flags and version list remain frozen. `packages/shared/src/site.ts::publicLanguages` additionally forces the captured default language to serve even if its enabled flag is false.
- `apps/server/src/lib/search-configuration.ts` defaults absent/invalid legacy search configuration to a cap of 12, bounds the request by that cap, selects an exact published version or its default, and searches every served language unless an explicit locale resolves to one locale/default. Selector-visibility controls are presentation-only.
- Native format 2 deliberately indexes only languages eligible at capture. A live flag cannot create postings for a language excluded from that immutable generation.
- Native promotion moves the active pointer and marks the old generation `superseded` **before** the publication consumer changes the new deployment to `READY`. Equating the public publication with the active pointer creates an availability gap and could choose an unpublished deployment. A newer deployment can also fail after its generation has been promoted.

The earlier compatibility matrix understated the live-overlay behavior. This ADR and the refreshed matrix correct that characterization; immutable public caps are a selected native policy, not a claim of legacy parity.

## User-selected policy

The user selected **published ceilings plus live restrictions**, rather than fully live controls or freezing both controls until republishing:

1. Content, version identity/defaults, language membership/defaults and the maximum eligible language set come from the selected immutable publication.
2. Effective locale eligibility is the intersection of published-enabled codes and same-project **currently enabled** language rows. Missing live rows restrict rather than resurrect content. A live enable/new locale cannot widen the published set.
3. Effective result limit is `min(requested_limit or effective_cap, published_cap, live_cap)`, with each configured cap an integer from 1 through 50 and a normal absent cap default of 12. Omitted limit uses the effective cap, not an unconditional 12.
4. Live controls can be relaxed again **within** the published ceiling: re-enabling a previously published-enabled locale or raising a temporarily lowered live cap back to that ceiling is allowed. Raising beyond the ceiling or adding a language excluded at capture requires a new reviewed publication.
5. Known malformed cap evidence fails closed instead of silently resetting to 12 and potentially raising a smaller intended limit. Unrelated rendering/presentation settings are not authority over selectors. Integer-valued JSON numbers such as `7`, `7.0` and `7e0` are treated identically.
6. An omitted locale searches the entire eligible intersection. An explicit eligible exact code selects only that code; unknown/disabled codes fall back to the **published** default only if it is still live-eligible. Otherwise the language set is empty. Never force a live-disabled default back into the corpus or choose an unrelated locale. Unknown/missing version selects the captured default version.

This is a bound relative to the publication, not an irreversible restriction over time. Changes are evaluated on each new database read; it is not a promise to cancel an already admitted in-flight request at the instant a setting changes.

## Technical decision

- Add feature-owned `prepare_published_search` and a one-shot `PreparedPublishedSearch::execute`. The prepared operation is neither `Clone` nor `Serialize`, has private admission/selection state, and must not be cached or reused as an authorization ticket. It accepts no client deployment/generation ID, filesystem path or page allow-list. A supplied workspace `Actor` must already be authenticated by a reviewed transport.
- Share a concrete SQLx connection with the existing read-only admission implementation. Admission, latest-READY selection and live caps/languages run in one short **REPEATABLE READ, READ ONLY** transaction. Commit before any snapshot hashing, index access or search. This read transaction is separate from, and does not change, publication capture's READ COMMITTED/advisory-lock/project-share-lock protocol.
- Select the highest-version READY deployment **before** left-joining its same-project, same-tenant registry row. A matching `ready` or `superseded` generation may be read only for that selected deployment. Never filter bad registry rows out before selecting the publication, because that would silently fall back to an older READY deployment.
- Do not use the active pointer as the public publication selector. It remains member-management/build state. Allowing the previous publication's `superseded` generation during the promotion gap is not a rollback mode, historical-generation selector, or legacy search fallback.
- Return concealed `None` for missing, denied, taken-down and never-published sites before index I/O. Execution of a missing, failed, incompatible, corrupt or mismatched latest-READY generation is a typed error. The eventual HTTP mapping is separately gated.
- Verify the selected full snapshot's canonical SHA-256 against its registry identity, share the typed format-2 scope validator with build extraction, fully validate the current manifest/runtime/index, and compare manifest document count/digest with the selected registry evidence.
- Apply the exact eligible-language **set** alongside project/deployment/version and reader-page terms inside Tantivy **before TopDocs**. Empty sets mean no hits, never unrestricted search. Recheck stored hit scope as defense in depth.
- Preparation spawns no task and has no filesystem-root argument. Execution is synchronous and consuming; the future transport must own a bounded, cancellation-aware, shutdown-drained blocking lifecycle. The local multithreaded test runtime uses `block_in_place` rather than creating a detached request task. No HTTP layer may simply execute this method on an async worker.

## Compatibility and operations

No snapshot, checksum, manifest, event or database format changes: snapshot/source **2**, manifest **3**, event **1**, required migrations through `202610160013`. No applied migration was edited and no retained artifact was relabeled. Existing compatible format-3 generations can be used unchanged. An invalid frozen control or a requested ceiling increase requires a new reviewed publication, not a historical snapshot edit.

Member-management request/result and active-pointer semantics are unchanged, including its pre-existing unavailable interval if the active generation's deployment is not READY. Public HTTP delivery, optional native/transition session transport, public schema/cache/error/analytics policy, reader JWT handoff, relevance/BM25 privacy, renderer compatibility and legacy removal remain gated.

**A superseded generation can still back the highest READY publication.** Future GC must protect that publication, in-flight readers, builds and recovery evidence; state/active-pointer membership alone is not deletion authority. No GC/rebuild/erasure workflow is introduced.

The current proof deliberately rehashes the full snapshot and validates the full generation. This is O(publication size) work and transfers the selected snapshot from PostgreSQL. Before public exposure, budget the **entire prepare-and-execute lifecycle**, including database transfer/memory, and verify a bounded/cached or materialized integrity-preserving design under largest-project load. No throughput, hard cancellation or load-readiness claim is made.

## Executed verification

- Scope/default/cap tests, malformed/ambiguous metadata refusal, redacted request Debug and exact pre-collector language/page intersection with a limit-1 denied-higher-rank fixture.
- Real PostgreSQL composition through registry registration/claim/build/promotion: published cap cannot be raised live; live lowering/disabling takes effect; excluded/new languages remain excluded; default flags changed in later drafts do not change published defaults; a disabled fallback gives an empty scope.
- A PostgreSQL lock-controlled interleaving changes live cap/language rows **after admission but before selection proceeds**. The in-flight operation keeps one old MVCC view; the next operation sees the new restrictions. No sleep-only timing assumption or production test hook is used.
- Promotion-before-READY and promoted-but-failed-newer-deployment proofs retain the correct previous READY publication. Missing registry evidence, failed state with valid files, changed snapshot, mismatched registry digest, changed runtime and missing files never fall back.
- Composed workspace/member override and reader page/zero-grant/revocation proofs preserve reader activity. Private/taken-down published sites are concealed even when the supplied index root cannot be opened.
- Final local formatter, locked all-target check, warnings-denied Clippy, **96 Cargo cases**, five separate Python integrity cases, architecture/shell checks, actual Rust **1.88.0** all-target compilation and the full non-Docker PostgreSQL **17.11** API/worker harness passed. All six conditional database entry functions ran non-skip in that harness. See [verification report](../../verification-report.md).
- Refreshed raw audit still exits 1 solely for inactive RSA `RUSTSEC-2023-0071`, with zero warnings; its guarded single-exception warning-denying audit passes. No search advisory exception was added.


## Query lifecycle follow-up — 2026-10-16

[ADR-0016](0016-supervised-search-query-lifecycle.md) now implements a feature-owned query service and application-driven driver for member HTTP and a non-HTTP published facade. Shared immediate admission, bounded transfer/validation, tracked SQL after caller cancellation, joined CPU work, coordinated API drain and bounded trace/metric privacy have real local DB/CPU/HTTP proofs. This does not expose public HTTP, resolve optional native/transition identity transport or prove hard CPU/I/O/drain/RSS/load bounds. The earlier milestone counts in this decision are historical; the current [verification report](../../verification-report.md) and [runtime guide](../../operations/search-query-runtime.md) carry the new evidence/limits. No new DDL, content policy or artifact format is introduced by this follow-up.
