-- Nibleaf Rust platform foundation
--
-- This is an expand-only migration applied AFTER the legacy Prisma migration
-- chain. It is deliberately not executed by API or worker startup. Operators
-- must apply it in a controlled migration job before rolling out Rust binaries.
--
-- Preconditions: PostgreSQL 14+; the current Nibleaf Prisma schema is present.

BEGIN;

-- Optimistic concurrency for autosaved page bodies. Existing rows begin at
-- revision zero; the Rust API requires the revision on every update.
ALTER TABLE page
    ADD COLUMN IF NOT EXISTS revision BIGINT NOT NULL DEFAULT 0;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'page_revision_nonnegative'
    ) THEN
        ALTER TABLE page
            ADD CONSTRAINT page_revision_nonnegative CHECK (revision >= 0);
    END IF;
END $$;

-- Path uniqueness was previously application-enforced. Fail safely before
-- adding the final database invariant rather than deleting/rewriting ambiguous
-- legacy documents during migration.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM page
        GROUP BY "projectId", "languageId", "branchId", path
        HAVING COUNT(*) > 1
    ) THEN
        RAISE EXCEPTION
            'cannot add page path uniqueness: duplicate (projectId, languageId, branchId, path) rows exist; remediate with the migration dry-run tool first';
    END IF;
END $$;
CREATE UNIQUE INDEX IF NOT EXISTS page_project_language_branch_path_key
    ON page ("projectId", "languageId", "branchId", path);

-- A composite candidate key lets onboarding bind its project and tenant with a
-- database-enforced relationship, preventing cross-tenant workflow records.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'project_id_organization_key'
    ) THEN
        ALTER TABLE project
            ADD CONSTRAINT project_id_organization_key UNIQUE (id, "organizationId");
    END IF;
END $$;

CREATE TABLE IF NOT EXISTS onboarding_run (
    id UUID PRIMARY KEY,
    project_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    state TEXT NOT NULL,
    created_by TEXT NOT NULL,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT onboarding_run_state_check CHECK (
        state IN ('database_initialized', 'external_provisioning', 'complete', 'failed', 'cancelled')
    ),
    CONSTRAINT onboarding_run_project_tenant_fk
        FOREIGN KEY (project_id, tenant_id)
        REFERENCES project (id, "organizationId") ON DELETE CASCADE
    -- `created_by` intentionally has no user FK: current Nibleaf account
    -- erasure must not be blocked by an audit/progress record.
);
CREATE INDEX IF NOT EXISTS onboarding_run_tenant_state_idx
    ON onboarding_run (tenant_id, state, created_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS onboarding_run_one_active_per_project
    ON onboarding_run (project_id)
    WHERE state IN ('database_initialized', 'external_provisioning');

-- Atomic intent for side effects and durable asynchronous work. There is no
-- exactly-once claim: consumers are intentionally at-least-once and must be
-- idempotent. A lease token fences stale workers after crash recovery.
CREATE TABLE IF NOT EXISTS outbox_event (
    id UUID PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES organization (id) ON DELETE CASCADE,
    aggregate_type TEXT NOT NULL CHECK (char_length(aggregate_type) BETWEEN 1 AND 120),
    aggregate_id TEXT NOT NULL CHECK (char_length(aggregate_id) BETWEEN 1 AND 191),
    event_type TEXT NOT NULL CHECK (char_length(event_type) BETWEEN 1 AND 160),
    schema_version SMALLINT NOT NULL CHECK (schema_version > 0),
    payload JSONB NOT NULL,
    correlation_id UUID NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    available_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    leased_until TIMESTAMPTZ,
    lease_token UUID,
    worker_id TEXT,
    last_error TEXT,
    delivered_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT outbox_event_state_check CHECK (state IN ('pending', 'processing', 'delivered', 'dead_letter')),
    CONSTRAINT outbox_event_lease_check CHECK (
        (state = 'processing' AND lease_token IS NOT NULL AND leased_until IS NOT NULL)
        OR (state <> 'processing')
    )
);
CREATE INDEX IF NOT EXISTS outbox_event_claim_idx
    ON outbox_event (available_at ASC, id ASC)
    WHERE state IN ('pending', 'processing');
CREATE INDEX IF NOT EXISTS outbox_event_tenant_created_idx
    ON outbox_event (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS outbox_event_dead_letter_idx
    ON outbox_event (created_at DESC)
    WHERE state = 'dead_letter';

-- At most one new deployment can build for a project. This is duplicated by an
-- advisory transaction lock in Rust so callers receive a deterministic 409;
-- the index is the final integrity layer.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM deployment
        WHERE status IN ('PENDING'::"DeploymentStatus", 'BUILDING'::"DeploymentStatus")
        GROUP BY "projectId"
        HAVING COUNT(*) > 1
    ) THEN
        RAISE EXCEPTION
            'cannot add one-active-deployment invariant: concurrent legacy deployments exist; resolve them before migrating';
    END IF;
END $$;
CREATE UNIQUE INDEX IF NOT EXISTS deployment_one_active_per_project
    ON deployment ("projectId")
    WHERE status IN ('PENDING'::"DeploymentStatus", 'BUILDING'::"DeploymentStatus");

COMMIT;
