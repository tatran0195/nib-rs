# Native public search: staged candidate and release runbook

**Current:** native API plus explicitly selected BFF/UI path locally verified; no production cutover. Whole-backend/multi-host readiness remains blocked; see [the current ledger](../production-readiness.md) and [ADR-0019](../architecture/decisions/0019-native-authority-contracts-and-owned-worker-drain.md).

[ADR-0017](../architecture/decisions/0017-public-search-contract-and-ranking-privacy.md) records the four user decisions. [ADR-0016](../architecture/decisions/0016-supervised-search-query-lifecycle.md) still defines cooperative execution, not hard resource guarantees. See the [verification report](../verification-report.md) for final logs and the wider legacy-app test limitation.

## Opt-in contract

`NIBLEAF_PUBLIC_SEARCH_ENABLED=true` explicitly adds **GET `/v1/public/sites/{project_id}/search`**. Absent defaults false; empty, malformed and non-Unicode values fail startup. Do not enable it on a production ingress merely because local checks pass. Native identity remains independently gated by `NIBLEAF_NATIVE_IDENTITY_ENABLED`.

- Parameters: `q` defaults blank; optional `lang`, `version`, `limit` (integer 1–50; equivalent numeric integer forms accepted). Unknown/duplicate parameters fail closed. Selectors have a documented 191-UTF-8-byte operational bound. No deployment/generation/path/page-allow-list input is accepted.
- User-selected query policy: **200 UTF-16 code units**, validated explicitly in Rust and TypeScript. That permits up to 600 UTF-8 bytes; the member diagnostic route remains nonblank/512 bytes. Blank and tokenless literal queries produce no hits after admission/integrity gates.
- **Corrected legacy characterization:** supported-Node execution of installed Zod 4.5.4 proves `.max(200)` counts Unicode scalars, not UTF-16 units. The selected native limit is therefore stricter for supplementary characters; it is an explicit staged compatibility difference, not exact Unicode validation parity. A golden adapter test records both behaviors. The prior assessment's UTF-16 claim was wrong.
- Public text is literal lexical input, not the Tantivy query language. Native title/headings/description/body boosts and Japanese/Chinese Lindera analysis are characterized; fuzzy/Arabic/semantic/relevance/score parity with Orama/Qdrant is not promised.
- Response: `{ "data": { "hits": [...], "runtime": "tantivy", "capabilities": { "answer": false } } }`. Hits preserve `id,title,path,description,icon?,snippet,score,language`. Internal publication/generation/version metadata is not exposed through this envelope. Snippets are plain text; clients must retain safe text rendering.
- Missing/unpublished/taken-down/unauthorized sites: concealed 404. Explicit invalid workspace credentials: 401, never anonymous downgrade. Invalid admitted parameters: 422. Capacity/deadline/data/integrity/encoding failures: existing 503 `dependency-unavailable`, no older-publication/legacy fallback or partial hits.
- All public-prefix success/error/method/path/identity/timeout responses carry **private, no-store** and **Vary: Cookie, Authorization**. Public JSON is serialized in the owned blocking job through an actual **1 MiB byte-limited writer**; it is not truncated or serialized as potentially large work on an async HTTP worker. Slow response buffering/global ingress/headers still need their own resource controls.

## Identity and scoring invariants

A narrow app-composed identity port runs on the driver-owned connection **inside** READ ONLY / REPEATABLE READ, after capacity and LOCAL statement timeout. Only the exact registered public transport bypasses the ordinary write-capable native middleware. There is no prefix-wide authentication bypass and other unsafe feature CSRF rules are unchanged.

Anonymous/public, live workspace membership, project reader cookies, reader page/zero grants and workspace override are supported. Native opaque cookies and one explicit migration bearer or `nibleaf_session` credential are exclusive and bounded. Native cookies cannot authenticate while their rollout gate is off. The adapter does not magically exchange a deployment-specific signed Better Auth cookie: the reviewed caller/proxy must forward native identity or the explicit authenticated transition credential. Reader JWT exchange/JWKS remains the existing separate handoff; a reader JWT is never a workspace Actor.

Native read-only authentication shares live account/session/support checks with the normal authenticator. Source inspection found the old authenticator did not actually verify the originating operator session. Both paths now require its live direct session and correlated active support context; stale CSRF rotation is fenced too, and a nullable operator FK cannot turn a support session into a direct session. Real PostgreSQL regression tests cover origin revocation and READ ONLY operation. No native or reader activity writes are added to public preparation.

BM25 document count, exact analyzer token totals and term document frequencies come only from the permitted version/language/page corpus. Tantivy's normal per-document length quantization remains. A stable title/page key participates **inside TopDocs before truncation**. Adversarial fixtures change 72 excluded pages/versions/languages and internal document order, asserting exact authorized score bits/order at limits 1/2/50. Trusted admitted-reader primitives also use scoped statistics; unrestricted member diagnostics retain their separate behavior.

This proves **content/score/order invariance for valid excluded corpus changes within resource bounds**, not absence of timing, availability, whole-generation size, operator/host or resource-pressure side channels. Full snapshot/generation integrity scanning remains O(publication size); server JSONB rendering precedes the transfer gate. No cached authorization ticket or stale scoped-statistics cache is used.

## Analytics and caller migration

Native search emits bounded operational outcomes/latency only, including `nibleaf_public_search_responses_total{outcome=hits|zero}`. No raw terms, user/project labels, project term-dashboard feed or durable query event/outbox is introduced. This is the user's explicit analytics retirement choice, not legacy dashboard parity.

`apps/app/src/lib/native-search-client.ts` is selected by the real search BFF/UI only under trusted `NIBLEAF_AUTHORITY=native` plus `NIBLEAF_NATIVE_API_ORIGIN`; transition remains the explicit pre-cutover authority. Native errors never retry legacy. The adapter has an independent bounded transport deadline, abort forwarding and error-stream cleanup. It requires a trusted HTTP(S) origin, validates typed inputs/outputs/capabilities, forwards only visitor Cookie/Authorization, propagates abort, forbids redirects, enforces no-store/credential variation and 1 MiB response bytes, and never tries another engine. Native adapter/gateway/server-handler/UI tests and focused strict TypeScript pass on Node 22.23.3/pnpm 10.30.3; the current verification report records complete app discovery/execution rather than the old incomplete suite attempt.

The actual `functions/site-search.ts` now forwards original request identity/abort with private BFF headers in native authority. Its hits-only export remains for transition consumers, while the search UI consumes capabilities, enforces the selected native input bound, hides answer UI, suppresses native search-click analytics and does not display old native results during revalidation/denial. Actual native result metadata conservatively overrides stale transition metadata. The native answer server function fails closed before a legacy call.

This is not native authentication/delivery/whole-backend client parity. The existing Better Auth browser/admin client and full site/publication adapter still require migration; normal production authority is untouched and every native error remains fail closed. The complete per-file app suite is now exercised through a memory-bounded runner; historical SIGKILL evidence remains, not a current unresolved assertion failure.

## Operator commands and safe recovery

Build from `rust/`: `cargo build --locked --workspace --bins`.

```bash
# No database writes. This verifies exact highest-READY selection, tenant join,
# canonical snapshot, registry document identity and full runtime/index evidence.
NIBLEAF_DATABASE_URL='from-secret-manager' \
  ./target/debug/search-ops verify-selected /private/shared/search PROJECT DEPLOYMENT

# Raw filesystem evidence only; use verify-selected for DB/publication binding.
./target/debug/search-ops verify ROOT PROJECT DEPLOYMENT GENERATION_UUID

# Fresh output root MUST NOT exist. Set private permissions for retained bodies.
# No serving directory, pointer, historical snapshot or DB row is overwritten.
umask 0077
NIBLEAF_DATABASE_URL='from-secret-manager' \
  ./target/debug/search-ops rebuild-new-root /private/recovery/new-root PROJECT DEPLOYMENT
```

Rebuild uses the same verified immutable snapshot, generation UUID, typed source extractor and source digest as the current registry. Existing output roots/symlinks are refused, including a repeat. Failure can leave a quarantined **new** root for diagnosis; never route it without verification. Deployment/volume replacement requires coordinated writer/reader quiescence and replica verification; the command does not perform that step. It is not GC, tenant purge, provider erasure or recovery of incompatible old snapshots. No delete/force-overwrite command exists.

The local eight-command PostgreSQL harness measures a **5,000-document / 5,415,930-byte snapshot** workload, three sequential queries, an eight-request burst (two complete/six Busy), verified new-root rebuild, existing-root refusal, corruption fail-closed and usable rebuilt data. The report preserves actual timings and Linux test-process high-water memory; this is neither cold-disk nor largest-production throughput proof.

A real TCP proxy blackholes completion of lock-blocked SQL. The response deadline returns 503 while graceful shutdown still waits; resuming the network allows a successful drain. A second round requires external SIGKILL and is explicitly recorded as **not** an application drain. Configure/stage the real process manager's termination and memory/CPU/ingress policies; no hard drain/CPU/I/O/RSS guarantee is claimed.

## Executable release preflight, not approval

From the repository root:

```bash
python3 scripts/native-search-cutover-preflight.test.py
NIBLEAF_DATABASE_URL='operator-readonly-url' python3 scripts/native-search-cutover-preflight.py \
  --index-root /private/shared/search --output cutover-inventory.json
# Exit 2 is intentional while blockers remain. Never feed this into an auto-deploy.
```

The tool uses READ ONLY SQL with LOCAL timeout, bounded non-truncated project inventory and optional full `verify-selected` subprocesses. It hashes the actual remaining Orama/Qdrant/runtime/job/provider references and lists ordered removal actions. The current staged source has **104 matching legacy files**; they are not removed or used as native fallback. `ready_for_production` is always false until an external reviewed release process supplies its own authority; a local scan cannot certify production.

Remaining release gates: actual project/publication inventory and consciously reviewed republishing; explicit caller/UI/session handoff and both-engine removal; selected Unicode/relevance acceptance; native retained-volume writer fence/GC/erasure and cross-runtime deletion coordination; real PostgreSQL+volume backup restore; largest production corpora/concurrency, replica/storage/network and forced-termination drills; broader app/renderer compatibility; production thresholds/ownership and release approval. Sole-tenant erasure stays blocked. No production or container/hosted-CI execution is claimed.

## Multi-host scope update

The user now requires the **entire backend on multiple hosts**, not a search-only release. Portable artifact envelope 1/import/export and two local unshared-root APIs are implemented; authoritative remote publication/download/cache, replica catalog/fences and offline retirement are not. See [ADR-0018](../architecture/decisions/0018-multi-host-search-distribution-and-retirement.md). Local archive success must not be described as qualified NFS, S3 durability, complete backup coverage or replica erasure.
