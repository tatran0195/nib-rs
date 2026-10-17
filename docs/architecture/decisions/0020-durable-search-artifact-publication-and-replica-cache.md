# ADR-0020: durable artifact publication and bounded replica materialization

**Status:** implementation/verification in progress; no cutover/production readiness claim  
**Date:** 2026-10-16

## Decision and intended implementation

Advance ADR-0018's operator-only artifact envelope into the native publication/query lifecycle. Use established Apache Arrow `object_store` 0.14.2 with the S3 client (declared MSRV 1.85; actual workspace 1.88 remains an executed gate). Do not implement custom S3 signing, credential discovery or a new microservice. Explicit configured credentials/endpoint/bucket, HTTPS by default, bounded transport and single-attempt conditional creates; no IMDS/ambient credential or local/older-generation fallback.

A new additive migration freezes each deployment's local/object delivery target, makes the generation target match it, records immutable exact artifact identity and durable upload attempts, and refuses object READY without matching verified artifact evidence. Publication payload/schema and native snapshot/source2/manifest3/envelope1 stay unchanged; mode is server-captured relational metadata, not browser input or a historical relabel. Existing local rows remain local and require deliberate republishing for object mode.

Workers own build/export/upload/content verification/lease renewal/promotion through completion. At-least-once retries reuse conditional immutable object keys and verify actual content, not ETag alone. Persist the attempted object scope before sending bytes; timeouts/lease loss are not proof the provider cancelled. Unknown/orphan attempt evidence remains retained. Do not advance READY before verified attachment in the same fenced promotion transaction.

API preparation keeps fresh identity/tenant/reader/role/grants, highest READY selection and live restrictions in its existing leased READ ONLY MVCC view. Only after admission does object mode resolve the server-selected immutable reference. A private unshared replica root, explicit replica UUID, process lock, owned marker and bounded single-flight/LRU disk lifecycle prevent uncontrolled duplicate materializations/overwrites. No authorization ticket/statistics/result cache is introduced: full source/integrity and exact query scope continue per request. In-use roots cannot be evicted. Interrupted/failed materialization is joined and cleaned or latched unsafe; cleanup failure is not called purged.

Track physical materializations durably by opaque replica/materialization IDs. Startup/shutdown purge only owned cache entries and receipt success after actual local cleanup. Unknown files/owners and changed replica identity fail closed; TTL/host expiry does not prove offline data erasure. Remote/local/cache/upload records and backups remain managed tenant data. No deletion or provider erasure endpoint is authorized by this work. Legacy account/tenant deletion must preflight the object-distribution marker before any external cleanup so relational restriction cannot become a late partial-erasure failure.

## Verification required

- Locked compile/lint/tests and real Rust1.88; dependency provenance and refreshed warnings-denied advisory policy.
- Real PostgreSQL mode/scope/FK/immutability/READY and stale-token/attempt/promotion races.
- S3 HTTP contract with explicit fake credentials and faults: conditional put, duplicate/existing-content integrity, ambiguous upload/retry, malformed/corrupt/oversized/missing objects, no ambient credential fallback or sensitive DEBUG logging. A local emulator/protocol fixture is not production S3/versioning/object-lock/durability qualification.
- Real API/worker publication with READY withheld until verified remote attachment; two cold unshared replica roots, fresh grants/revocation, missing/corrupt selected evidence no fallback, in-use eviction/single-flight/quota/cleanup and cancellation/drain proof.
- Fail-closed preflight before legacy/native retained/relational deletion. No offline replica, unknown remote write, retained backup or provider lifecycle is represented erased.

The previous 124-Cargo/525-app receipt remains historical after source changes. Whole-backend/multi-host/provider/remaining-feature gates remain open until actually implemented and verified.
