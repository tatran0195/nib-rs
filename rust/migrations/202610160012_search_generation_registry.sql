-- Durable lifecycle for immutable project-scoped Tantivy generations.
--
-- This is additive and provides the constrained registry later used by the
-- publication slice. A publication transaction records a matching durable
-- request, and READY is withheld until the row is promoted. A failed/reclaimed
-- filesystem build therefore cannot advance a project reader to an unvalidated
-- generation.

BEGIN;

-- A composite FK makes it impossible for a generation row to bind a deployment
-- from another project. The deployment primary key already proves `id` is
-- unique; the explicit pair is the PostgreSQL reference target for that tenant
-- scope invariant.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'deployment_id_project_key'
    ) THEN
        ALTER TABLE deployment
            ADD CONSTRAINT deployment_id_project_key UNIQUE (id, "projectId");
    END IF;
END $$;

CREATE TABLE rust_search_generation (
    generation_id UUID PRIMARY KEY,
    project_id TEXT NOT NULL,
    deployment_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    -- Canonical type-tagged SHA-256 of the full JSON deployment snapshot. It
    -- is calculated before/at registration and survives PostgreSQL JSONB key
    -- ordering so a worker cannot build from a substituted snapshot.
    snapshot_sha256 TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0,
    lease_token UUID,
    leased_until TIMESTAMPTZ,
    worker_id TEXT,
    document_count INTEGER,
    documents_sha256 TEXT,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT rust_search_generation_project_tenant_fk
        FOREIGN KEY (project_id, tenant_id)
        REFERENCES project (id, "organizationId") ON DELETE CASCADE,
    CONSTRAINT rust_search_generation_deployment_project_fk
        FOREIGN KEY (deployment_id, project_id)
        REFERENCES deployment (id, "projectId") ON DELETE RESTRICT,
    CONSTRAINT rust_search_generation_generation_project_key
        UNIQUE (generation_id, project_id),
    CONSTRAINT rust_search_generation_snapshot_sha256_check
        CHECK (snapshot_sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT rust_search_generation_documents_sha256_check
        CHECK (documents_sha256 IS NULL OR documents_sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT rust_search_generation_attempts_check CHECK (attempts >= 0),
    CONSTRAINT rust_search_generation_document_count_check
        CHECK (document_count IS NULL OR document_count > 0),
    CONSTRAINT rust_search_generation_state_check
        CHECK (state IN ('pending', 'building', 'ready', 'superseded', 'failed')),
    CONSTRAINT rust_search_generation_lifecycle_check CHECK (
        (state = 'pending'
            AND lease_token IS NULL AND leased_until IS NULL AND worker_id IS NULL
            AND document_count IS NULL AND documents_sha256 IS NULL AND completed_at IS NULL)
        OR (state = 'building'
            AND lease_token IS NOT NULL AND leased_until IS NOT NULL AND worker_id IS NOT NULL
            AND document_count IS NULL AND documents_sha256 IS NULL AND completed_at IS NULL)
        OR (state IN ('ready', 'superseded')
            AND lease_token IS NULL AND leased_until IS NULL AND worker_id IS NULL
            AND document_count IS NOT NULL AND documents_sha256 IS NOT NULL AND completed_at IS NOT NULL)
        OR (state = 'failed'
            AND lease_token IS NULL AND leased_until IS NULL AND worker_id IS NULL
            AND last_error IS NOT NULL AND completed_at IS NOT NULL)
    )
);

-- One immutable generation represents one immutable deployment. Replays reuse
-- this exact row and UUID, never produce a competing final filesystem path.
CREATE UNIQUE INDEX rust_search_generation_project_deployment_key
    ON rust_search_generation (project_id, deployment_id);
CREATE INDEX rust_search_generation_claim_idx
    ON rust_search_generation (state, leased_until ASC, created_at ASC)
    WHERE state IN ('pending', 'building');
CREATE INDEX rust_search_generation_project_state_idx
    ON rust_search_generation (project_id, state, created_at DESC);

-- This one-row-per-project pointer is the sole reader selection authority.
-- Promotion updates the ready generation and this pointer in one transaction;
-- historical superseded generations remain retained for a later explicit GC.
CREATE TABLE rust_search_active_generation (
    project_id TEXT PRIMARY KEY REFERENCES project (id) ON DELETE CASCADE,
    generation_id UUID NOT NULL,
    activated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT rust_search_active_generation_generation_project_fk
        FOREIGN KEY (generation_id, project_id)
        REFERENCES rust_search_generation (generation_id, project_id) ON DELETE RESTRICT
);

COMMIT;
