# ADR-0019: native authority, wire contracts and owned worker shutdown

**Status:** implementation locally verified; whole-backend/client/provider cutover remains blocked  
**Date:** 2026-10-16

## Native authority is not perpetual migration fallback

`NIBLEAF_AUTHORITY` accepts exactly `transition` (the current pre-cutover default) or `native`. Native requires `NIBLEAF_NATIVE_IDENTITY_ENABLED=true`; empty/malformed/non-Unicode or inconsistent configuration fails startup. Native mode never resolves private Actors from the Better Auth `session` table/migration bearer/cookie. The public optional-identity port likewise rejects an explicit disabled migration credential rather than silently treating it as anonymous. Native cookies and legitimately anonymous PUBLIC requests remain supported.

The Node search BFF reads the same trusted server authority plus `NIBLEAF_NATIVE_API_ORIGIN`; browser input never supplies backend selection/origin. Native errors/cancellation never invoke a legacy search/answer path. Transition is an explicit release stage preserving current authority, not a per-request fallback/shadow mode. The native path now runs through the real server-function, query hook and search UI: request identity/abort forwarding, private response headers, bounded transport/body, `tantivy`/`answer:false`, UTF-16 validation and native answer/click-analytics suppression. Actual result capabilities also override stale transition metadata conservatively.

This does **not** finish native browser/admin authentication, instance metadata, the full original delivery/API/client feature set or Better Auth removal. Whole-backend readiness must reject partial coverage, retain unrelated product behavior, migrate consumers and then remove the transition dependency/code in the reviewed release. Native authority is not permission to route unknown legacy features to a partial backend.

## Date-time is a wire contract

Existing OpenAPI declares string/date-time for member creation, invitation/session/OTP/support expiry. Native DTOs previously relied on `time::OffsetDateTime` default serde representation without human-readable serialization. Those HTTP DTO fields now use explicit `time::serde::rfc3339`; workspace `time` explicitly declares the already-resolved well-known helper feature. Token claims/DB timestamps and epoch/HMAC verification are not globally reformatted. Unit and real HTTP tests prove string values; this is a documented-contract repair, not a silent change to signed token formats.

## A joined inner function was not enough for worker ownership

Source review found the worker's top-level `tokio::select!` dropped the dispatch/health futures on signal/error, and `try_join!` could drop a peer loop. That could drop a dispatch JoinSet while a joined `spawn_blocking` generation build was still running, contradicting the earlier whole-lifecycle claim even though the inner build function awaited its handle.

The worker now owns both HTTP and worker-loop futures with cancellation plus `join!`, and dispatch/reconciliation use the same private two-future supervision. Signals stop new claims but accepted SQL/provider/CPU work and acknowledgement complete before pool closure and the `worker HTTP and accepted work drained` log. Peer errors cancel the other loop **without dropping it**. The large worker future is boxed, not detached; no generic mediator/saga/repository is introduced.

A unit holds real blocking work after a peer error; another holds accepted async work after cancellation. A real disposable-DB process test blocks an accepted onboarding SQL delivery, signals SIGTERM, proves the process stays alive, releases the lock, and asserts persisted `delivered` acknowledgement and normal drain. Forced termination can still leave reclaimable work, and external delivery remains at-least-once. No hard failed-network/CPU/I/O/drain bound or exactly-once external-effect claim is made.

## Verification and remaining scope

The current report distinguishes complete per-file app assertion execution under a bounded two-GiB runner, native API/replica/authority process proofs, strict Rust/MSRV/supply-chain gates, optimized binaries and scoped disposable DB/artifact recovery from unqualified production/provider behavior. Full multi-host artifact publication/cache/retirement, managed-store erasure/fences, the remaining original feature inventory and real environment qualification remain open; see [whole-backend readiness](../../production-readiness.md) and [multi-host design](0018-multi-host-search-distribution-and-retirement.md).
