# ADR-0017: public search contract, scoped scoring and staged migration

**Status:** implemented and locally verified staged candidate, disabled by default; production release blocked (2026-10-16)

## Decisions explicitly selected by the user

1. Preserve the legacy public envelope/parameter names with an explicitly selected native Unicode policy: `q`/`lang`/`version`/`limit`, `q` defaults blank, at most 200 UTF-16 code units, blank query yields no hits. Return `{data:{hits,runtime,capabilities}}` with typed lexical-runtime metadata and `answer:false`. Keep IDs/rendering fields and numeric scores; this does not promise Orama/semantic/fuzzy score parity.
2. Compute BM25 statistics only over the permitted publication version, languages and reader pages. Apply deterministic tie-breaking **before** the result limit. Excluded documents must not change authorized scores/order. Use Tantivy's existing statistics-provider/collector APIs, not a fork of the engine.
3. Use aggregate operational metrics only: no raw terms, user/project labels or native project search-term dashboard feed. This intentionally retires native raw-term collection, not silently claims analytics parity.
4. Deliver a staged cutover candidate. Implement/test the distinct opt-in native route and caller adapter and an executable migration/removal preflight. Keep current production authority untouched until reviewed publication inventory, compatibility, operations and release approval. No automatic fallback/shadow mode; no production routing or destructive data deletion.

## Implementation constraints

- A public feature router remains disabled by default. Global native write-capable authentication must not run before this handler's feature lease or enter its read-only transaction. An app-composed, narrow optional-identity resolver runs on the driver-owned connection inside the same short READ ONLY / REPEATABLE READ view as reader admission/selection. Core/search do not depend on identity. Native and transition credentials are explicit, exclusive, bounded and fail closed; reader cookies/JWTs never manufacture workspace Actors.
- Public queries are literal lexical text, not a browser-controlled Tantivy query language. Mandatory scope filters remain before collection. The existing diagnostic/member query behavior is independent.
- Legacy UTF-16 validation is not the previous 512-byte internal assumption: 200 BMP characters can need 600 UTF-8 bytes. Public transport/library validation must represent that without truncation; management retains its 512-byte bound.
- All public success/error/overload/routing responses require private/no-store and credential variation. Conceal missing/unpublished/taken-down/unauthorized sites as 404; do not return old/broken publications or another engine.
- Score/content/order privacy is a specific verified invariant, not a claim to eliminate timing, resource-pressure or host-level side channels. ADR-0016 cooperative resource/network/panic-hook limitations remain unless separately implemented and proved.
- Local load/fault/rebuild/retention tests can establish only their measured environment. Real-backup recovery, largest production corpora, production thresholds/ownership, full renderer/client release and cross-store tenant erasure cannot be certified from the sandbox.

## Work checklist

- [x] Optional native/transition identity, typed opt-in HTTP, strict public inputs/response/cache/error contract.
- [x] Scoped statistics, deterministic pre-limit collector and adversarial score/order invariance tests.
- [x] Literal UTF-16/blank/multilingual request behavior and caller adapter tests on supported Node.
- [x] Actual PostgreSQL and HTTP access-mode/revocation/grants/selection/overload/privacy tests.
- [x] Reproducible local load, cancellation/fault and safe recovery/preflight evidence.
- [x] Exact legacy inventory and executable migration/removal gates; no live switch or automatic fallback.
- [x] Final formatter/check/strict Clippy/tests/MSRV/dependency/audit/docs/evidence refresh.

Local completion is limited to the implemented candidate and recorded tests, not production/caller/renderer/retained-store completion. The unselected caller adapter has 13 passing tests/strict types; the broader original app suite was attempted with generated i18n but killed by SIGKILL, and is not green. Supported-Node execution also corrected the legacy Unicode assumption: installed Zod 4.5.4 max counts scalars; the explicitly selected native UTF-16 bound is a staged compatibility difference. See the [candidate runbook](../../operations/public-search-candidate.md) and current verification report. The prior 109-case receipt is historical for this batch.
