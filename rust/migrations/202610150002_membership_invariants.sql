-- Membership ownership invariants for Rust-managed project tenants.
--
-- Apply after 202610130001_rust_platform.sql and the complete legacy Prisma
-- migration chain. This migration is expand-only: it fails on anomalous legacy
-- rows rather than selecting an owner or rewriting authorization data.

BEGIN;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM member
        WHERE role NOT IN ('owner', 'admin', 'member') OR role IS NULL
    ) THEN
        RAISE EXCEPTION
            'cannot constrain membership roles: member rows with an unsupported role exist; remediate deliberately before enabling Rust membership routes';
    END IF;

    IF EXISTS (
        SELECT 1
        FROM project AS p
        LEFT JOIN member AS m
            ON m."organizationId" = p."organizationId"
           AND m.role = 'owner'
        GROUP BY p.id
        HAVING COUNT(m.id) <> 1
    ) THEN
        RAISE EXCEPTION
            'cannot enforce project ownership invariant: each project organization must have exactly one owner; resolve missing or duplicate owners deliberately before migration';
    END IF;
END $$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'member_role_supported'
    ) THEN
        ALTER TABLE member
            ADD CONSTRAINT member_role_supported
            CHECK (role IN ('owner', 'admin', 'member'));
    END IF;
END $$;

-- This index prevents duplicate owners even if a writer outside Rust bypasses
-- the advisory lock. The Rust transfer transaction demotes existing owner rows
-- before promoting the selected admin, so it remains valid throughout transfer.
CREATE UNIQUE INDEX IF NOT EXISTS member_one_owner_per_organization
    ON member ("organizationId")
    WHERE role = 'owner';

COMMIT;
