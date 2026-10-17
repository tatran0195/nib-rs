# Legacy data migration strategy

## Scope and safety posture

The Rust platform currently **shares the legacy Nibleaf PostgreSQL database**. It does not copy, delete or transform production data. `rust/migrations/202610130001_rust_platform.sql` is an expand-only compatibility migration on top of the current Prisma migration chain.

Never test this procedure against a production database. Take a verified restore-capable backup first.

## Preconditions

1. Record the legacy application image/revision and database schema migration state.
2. Backup PostgreSQL and rehearse restore to an isolated environment.
3. Run duplicate discovery before applying the uniqueness migration:

```sql
SELECT "projectId", "languageId", "branchId", path, COUNT(*)
FROM page
GROUP BY 1, 2, 3, 4
HAVING COUNT(*) > 1;

SELECT "projectId", COUNT(*)
FROM deployment
WHERE status IN ('PENDING', 'BUILDING')
GROUP BY 1
HAVING COUNT(*) > 1;

SELECT p.id AS project_id, p."organizationId", COUNT(m.id) AS owner_count
FROM project AS p
LEFT JOIN member AS m
  ON m."organizationId" = p."organizationId" AND m.role = 'owner'
GROUP BY p.id, p."organizationId"
HAVING COUNT(m.id) <> 1;

SELECT id, "organizationId", role
FROM member
WHERE role NOT IN ('owner', 'admin', 'member') OR role IS NULL;
```

4. Remediate duplicates or unsupported roles deliberately with product owners. The migrations intentionally abort rather than choosing an owner, silently changing authorization data, or deleting documents.
5. Verify existing legacy migrations are completely applied before the Rust migrations.

## Expand phase

Apply the file with the same restricted migration role used for schema changes:

```bash
for migration in rust/migrations/*.sql; do
  psql "$NIBLEAF_DATABASE_URL" -v ON_ERROR_STOP=1 -f "$migration"
done
```

The current migration set adds:

- `page.revision BIGINT NOT NULL DEFAULT 0` plus non-negative check;
- unique page path index scoped by project/language/branch;
- `onboarding_run` bound by a composite project/tenant FK;
- `outbox_event` with state, lease and retry constraints;
- one-active-publication partial index;
- a constrained `member.role` vocabulary and a partial unique index enforcing at most one `owner` per organization;
- inert additive `member_invitation` and `member_invitation_delivery` foundations for ADR-0007: project/organization composite scope, canonical recipient/role/status/idempotency/delivery constraints, delivery lease fields, and no raw acceptance token.

The Rust invitation lifecycle now writes only its additive tables and does **not** replace, read, or mutate the legacy Better Auth `invitation` table. Legacy IDs and links remain authoritative until the separately documented [dashboard adapter/coexistence cutover](rust-invitation-cutover.md); Rust routing is not a token/data migration signal.

The migration is not run automatically by API or worker startup. Both binaries refuse readiness when `outbox_event` is absent; the API additionally requires the membership owner-invariant index artifact.

## Reconciliation checks

After applying in staging, run:

```sql
SELECT to_regclass('public.outbox_event'), to_regclass('public.onboarding_run');
SELECT COUNT(*) AS invalid_page_revisions FROM page WHERE revision < 0;
SELECT "projectId", "languageId", "branchId", path, COUNT(*)
FROM page GROUP BY 1,2,3,4 HAVING COUNT(*) > 1;
SELECT "projectId", COUNT(*)
FROM deployment
WHERE status IN ('PENDING','BUILDING') GROUP BY 1 HAVING COUNT(*) > 1;
SELECT p.id AS project_id, p."organizationId", COUNT(m.id) AS owner_count
FROM project AS p
LEFT JOIN member AS m
  ON m."organizationId" = p."organizationId" AND m.role = 'owner'
GROUP BY p.id, p."organizationId"
HAVING COUNT(m.id) <> 1;
SELECT to_regclass('public.member_one_owner_per_organization');
```

Then use disposable legacy sessions to exercise: create project, create/update page with a revision, change an admin/member role, transfer ownership to an existing admin, publish, expire a worker lease, and verify the worker completes the event. `rust/scripts/test-postgres-integration.sh` automates this against an explicitly named disposable test database.

## Cutover order for an implemented slice

1. Deploy the migration job and validate checks.
2. Deploy a Rust worker with no Rust traffic. Confirm readiness and no unexpected dead letters.
3. Route an allowlisted/staging dashboard flow to Rust `/v1` through an adapter while retaining legacy fallback.
4. Compare resulting project/page/deployment rows and public snapshot output.
5. Gradually expand traffic only after feature and compatibility acceptance tests pass.
6. Keep legacy endpoint rollback available. The expand phase is additive, so rollback is an application routing rollback; dropping added schema is not part of an emergency rollback.

## Data deletion and retention

The Rust migration introduces no destructive delete. Legacy organization cascade semantics continue to remove `onboarding_run` and outbox rows through tenant foreign keys. Before routing legacy account/project deletion through Rust, add erasure integration tests covering ClickHouse, Qdrant, storage, exports, Git credentials and queued work.

## Rollback

- **Before routing traffic:** do not deploy Rust binaries; schema additions are inert.
- **After a bad Rust application deploy:** route traffic back to legacy application, stop Rust worker after a bounded drain, inspect pending/dead-letter events.
- **Do not drop `outbox_event`, `onboarding_run`, `page.revision`, or indexes during incident response.** They may contain evidence of committed work. A separately reviewed contract migration can remove them only after all writers/readers and backups no longer require them.
