# Native published-search scope and selection policy

> **Current staged-search delta — ADR-0017 (2026-10-16):** the distinct opt-in native public transport, in-budget native/transition read-only identity, selected UTF-16/literal contract, scoped BM25/stable pre-limit ties, bounded owned JSON, aggregate-only metrics, offline rebuild and local load/network/HTTP proofs are implemented. It is **disabled by default**, with no production routing, UI/caller selection or legacy removal. Earlier statements below that transport/scoring are unimplemented describe the prior milestone and are superseded by the [current candidate contract and release boundary](../operations/public-search-candidate.md). Selected 200-UTF-16 native validation differs from installed Zod 4.5.4 scalar counting; no exact Unicode parity claim.

**Status:** implemented and verified as a non-HTTP composition; public delivery/cutover remains gated  
**Date:** 2026-10-16  
**Decision:** user-selected **published ceilings plus live restrictions**, [ADR-0015](../architecture/decisions/0015-published-search-ceilings-and-ready-selection.md)  
**Related:** [compatibility matrix](../architecture/native-public-search-compatibility.md), [runtime upgrade](native-search-runtime-upgrade.md), [operations](../operations/native-search-generations.md), [verification](../verification-report.md)

## What is implemented

`prepare_published_search` derives fresh public/workspace/reader admission and selects the highest-version READY deployment, its exact same-project/same-tenant generation and current restrictions in one short read-only REPEATABLE READ transaction. It commits before returning an opaque, one-shot `PreparedPublishedSearch`.

`PreparedPublishedSearch::execute` is synchronous. It consumes the operation, verifies the full snapshot's canonical registry identity, resolves defaults and caps from that snapshot plus the captured live restrictions, validates the manifest/current runtime/index and registry count/digest, and intersects the exact version/language/page scope before the result collector. It is not a public route, a reusable authorization token, or a serialized response. [ADR-0016](../architecture/decisions/0016-supervised-search-query-lifecycle.md) now wraps this operation in an API-driven feature service/driver with bounded admission/validation, tracked SQL and joined shutdown. The direct preparation primitive still spawns no task; use the supervised facade for serving, not direct synchronous execution on an async worker.

The management route `GET /v1/projects/{project_id}/search` keeps its DTO, tenant/error precedence and active-pointer semantics; it now uses the supervised runtime and returns existing 503 Problem Details on operational limits/overload. No native public HTTP route is installed, and no legacy caller, Orama/Qdrant provider or production data was removed or switched.

## Explicit compatibility decisions

The inspected legacy `getPublished` action does **not** keep all search controls frozen: `overlayLiveChrome` overlays project configuration and matching language enabled flags before search resolves its cap/locales. The earlier matrix understated this. Native behavior is an explicit bounded policy, not claimed full legacy parity.

| Situation | Selected native library behavior |
|---|---|
| No version, empty/whitespace version, or unknown slug | Use the selected publication's exact default version ID/slug. Later draft default/name changes cannot redirect it. |
| No language or empty/whitespace language | Search every code in `published-enabled ∩ live-enabled`. New languages and previously excluded locales cannot enter an old corpus. |
| Explicit eligible code | Search that exact code; regional codes are distinct, not tokenizer-family hints. |
| Explicit unknown, disabled or excluded code | Fall back only to the captured default if it remains live-eligible. Otherwise the language set is empty; never substitute a different locale or force the default enabled. |
| Live language row deleted/missing | Exclude it. This is stricter than legacy's unmatched-row fallback. |
| Live language enabled again | It may re-enter only if it was eligible in this publication. Otherwise review and republish. |
| No request limit | Use the lesser published/live cap. An absent normal setting defaults to 12; this does not override an explicitly authored valid cap. |
| Supplied limit | Require 1–50, then use `min(request, published cap, live cap)`. |
| Live cap increases above the published cap | The published cap remains authoritative. A new reviewed publication is required to raise that ceiling. |
| Live cap decreases, then increases within the ceiling | Both are honored on subsequent reads. The policy is not an irreversible restriction over time. |
| Known malformed cap | Typed failure, not legacy's silent reset to 12. Repair live configuration; if the malformed value is frozen, review and republish. Never edit a historical snapshot to pass its checksum. |
| Hidden selectors / retired provider or answer settings | Do not remove a valid explicit language/version or reset a valid cap. Presentation controls are not authorization. No dense/answer capability is supplied by this lexical library. |
| A newer deployment is not READY, even if promoted | Continue selecting the prior highest READY deployment and its retained generation, even if that generation is `superseded`. |
| Highest READY lacks compatible registry/files/runtime/source evidence | Typed unavailable/integrity failure. Do not skip it in favor of older READY data, Orama or Qdrant. |
| Denied, taken-down, missing or never-published site | Concealed `None` before index access; eventual HTTP mapping is not yet installed. |

Controls are read on each preparation, with no authorization/settings cache. A change committed before the first transaction read is visible to that new request; an already admitted in-flight request keeps its consistent MVCC view. This is not instantaneous cancellation of in-flight work.

## Formats and retained data

This policy needs **no new DDL or artifact format**. Required migration remains `202610160013`; snapshot/source format remains **2**, current runtime-bound manifest **3**, event schema **1**. Existing compatible generations are not relabeled or rewritten. Follow the earlier source/runtime upgrade guides for snapshot 1 and manifest 1/2; a new reviewed publication is still required for those artifacts.

A superseded generation can still serve the highest READY publication during a promotion gap or after a newer promoted deployment fails. Do not delete it just because the active pointer changed. Automated safe GC/rebuild/purge is not implemented; native files/staging/backups remain retained tenant data and sole-tenant erasure remains fail-closed.

## Verification and staging review

Executed locally, without Docker:

- Unit coverage of exact/default selectors, published/live/request cap intersection, missing/disabled default, live-set exclusion, malformed/ambiguous controls and redacted request Debug.
- A denied higher-scoring locale/page cannot displace an authorized hit at limit 1; field-targeted query syntax cannot remove the language/page/version intersections.
- Real PostgreSQL registration/claim/build/promotion plus composed queries: live tightening, blocked widening, later-draft default changes, promotion-before-READY and newer failed deployment, absent/failed registry, snapshot/digest/runtime tampering, missing files, workspace override, reader page/zero/revoked scope and unchanged reader activity.
- A controlled PostgreSQL table lock pauses selection after admission; committed concurrent live changes do not mix into that prepared request, but appear in the next one.
- The complete final local source/MSRV and PostgreSQL/API/worker suite passed; see the current verification report for **109 Cargo cases**, seven separately non-skip conditional DB functions and the remaining inactive-RSA raw-audit finding.

Production staging must still review the actual published ceilings, excluded locales and live control state for every migrating project. Raising eligibility/caps means republishing the intended drafts consciously; native snapshots remain incompatible with the unadapted legacy renderer.

## Not public-cutover approval

Still required: optional native/transition actor/session transport and cookie handoff; public `q`/`lang`/version/limit names, blank/Unicode/error/response contract; credential-varying no-store policy; no raw-query logging and privacy-safe analytics; relevance and generation-wide BM25/score privacy review; client/renderer compatibility; largest-project/concurrent memory/latency and hard network/storage/forced-termination validation beyond the implemented cooperative execution/drain; volume recovery/rebuild/GC/erasure and operational drills; controlled caller migration and removal of both legacy engines.

The new proof currently transfers/rehashes the full selected snapshot and fully validates the generation. This is O(publication size), not a high-traffic design or benchmark. The implemented service now covers feature database preparation/transfer/validation and joined blocking ownership, but not preceding global auth/extraction or hard allocator/network/filesystem bounds. See [runtime configuration/caveats](../operations/search-query-runtime.md). Any future cache/materialization must preserve immutable identity and fresh live admission/restrictions. The existing library's nonblank/512-byte query bound is not a decision to replace legacy's public 200-UTF-16-unit/blank-query behavior.
