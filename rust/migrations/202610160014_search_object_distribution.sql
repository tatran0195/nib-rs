-- Immutable delivery identity and durable retained artifact/materialization facts.
-- Existing rows stay local. No historical snapshot/index relabel or provider
-- deletion occurs. Object rows deliberately block unreviewed relational erasure.
BEGIN;
ALTER TABLE deployment ADD COLUMN rust_search_delivery_mode TEXT NOT NULL DEFAULT 'local';
ALTER TABLE deployment ADD CONSTRAINT deployment_search_delivery_check CHECK (rust_search_delivery_mode IN ('local','object'));
ALTER TABLE deployment ADD CONSTRAINT deployment_search_delivery_scope_key UNIQUE (id,"projectId",rust_search_delivery_mode);
ALTER TABLE rust_search_generation ADD COLUMN delivery_kind TEXT NOT NULL DEFAULT 'local';
ALTER TABLE rust_search_generation ADD CONSTRAINT rust_search_generation_delivery_check CHECK (delivery_kind IN ('local','object'));
ALTER TABLE rust_search_generation ADD CONSTRAINT rust_search_generation_delivery_fk
    FOREIGN KEY (deployment_id,project_id,delivery_kind) REFERENCES deployment(id,"projectId",rust_search_delivery_mode);
ALTER TABLE rust_search_generation ADD CONSTRAINT rust_search_generation_distribution_scope_key
    UNIQUE (generation_id,project_id,tenant_id,deployment_id,snapshot_sha256);

CREATE TABLE rust_search_artifact_upload (
    generation_id UUID NOT NULL,
    project_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    deployment_id TEXT NOT NULL,
    snapshot_sha256 TEXT NOT NULL CHECK (snapshot_sha256 ~ '^[0-9a-f]{64}$'),
    artifact_sha256 TEXT NOT NULL CHECK (artifact_sha256 ~ '^[0-9a-f]{64}$'),
    artifact_bytes BIGINT NOT NULL CHECK (artifact_bytes > 0 AND artifact_bytes <= 536870912),
    documents_sha256 TEXT NOT NULL CHECK (documents_sha256 ~ '^[0-9a-f]{64}$'),
    document_count INTEGER NOT NULL CHECK (document_count >= 0),
    object_key TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending','verified','unknown')),
    claim_token UUID NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 1 CHECK (attempts > 0),
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (generation_id,artifact_sha256),
    CONSTRAINT rust_search_upload_scope_fk FOREIGN KEY (generation_id,project_id,tenant_id,deployment_id,snapshot_sha256)
        REFERENCES rust_search_generation(generation_id,project_id,tenant_id,deployment_id,snapshot_sha256) ON DELETE RESTRICT,
    CONSTRAINT rust_search_upload_key_check CHECK (object_key =
        'native-search/v1/' || encode(sha256(convert_to(project_id,'UTF8')),'hex') || '/' || generation_id::text || '/' || artifact_sha256 || '.tar.zst')
);

CREATE TABLE rust_search_artifact (
    generation_id UUID PRIMARY KEY,
    project_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    deployment_id TEXT NOT NULL,
    snapshot_sha256 TEXT NOT NULL,
    format_version SMALLINT NOT NULL CHECK (format_version=1),
    artifact_sha256 TEXT NOT NULL,
    artifact_bytes BIGINT NOT NULL,
    documents_sha256 TEXT NOT NULL,
    document_count INTEGER NOT NULL,
    object_key TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (generation_id,project_id,tenant_id,artifact_sha256),
    CONSTRAINT rust_search_artifact_upload_fk FOREIGN KEY(generation_id,artifact_sha256)
        REFERENCES rust_search_artifact_upload(generation_id,artifact_sha256) ON DELETE RESTRICT,
    CONSTRAINT rust_search_artifact_scope_fk FOREIGN KEY(generation_id,project_id,tenant_id,deployment_id,snapshot_sha256)
        REFERENCES rust_search_generation(generation_id,project_id,tenant_id,deployment_id,snapshot_sha256) ON DELETE RESTRICT
);

CREATE TABLE rust_search_replica_root (
    replica_id UUID PRIMARY KEY,
    root_id UUID NOT NULL UNIQUE,
    UNIQUE (replica_id,root_id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE rust_search_materialization (
    materialization_id UUID PRIMARY KEY,
    replica_id UUID NOT NULL,
    root_id UUID NOT NULL,
    generation_id UUID NOT NULL,
    project_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    artifact_sha256 TEXT NOT NULL CHECK (artifact_sha256 ~ '^[0-9a-f]{64}$'),
    state TEXT NOT NULL CHECK (state IN ('materializing','ready','quarantined','purged')),
    unpacked_bytes BIGINT CHECK (unpacked_bytes >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    purged_at TIMESTAMPTZ,
    CHECK ((state='purged')=(purged_at IS NOT NULL)),
    CONSTRAINT rust_search_materialization_root_fk FOREIGN KEY (replica_id,root_id) REFERENCES rust_search_replica_root(replica_id,root_id) ON DELETE RESTRICT,
    CONSTRAINT rust_search_materialization_artifact_scope_fk FOREIGN KEY (generation_id,project_id,tenant_id,artifact_sha256) REFERENCES rust_search_artifact(generation_id,project_id,tenant_id,artifact_sha256) ON DELETE RESTRICT
    -- No cascading tenant FK: unreachable/offline retained locations survive
    -- relational state and are never inferred purged from heartbeat/TTL.
);
CREATE INDEX rust_search_materialization_replica_idx ON rust_search_materialization(replica_id,state);
CREATE INDEX rust_search_materialization_project_idx ON rust_search_materialization(project_id,state);

CREATE FUNCTION rust_search_delivery_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_TABLE_NAME='deployment' THEN
        IF NEW.rust_search_delivery_mode IS DISTINCT FROM OLD.rust_search_delivery_mode THEN
            RAISE EXCEPTION 'native search delivery identity is immutable' USING ERRCODE='23514';
        END IF;
    ELSE
        IF NEW.delivery_kind IS DISTINCT FROM OLD.delivery_kind THEN
            RAISE EXCEPTION 'native search delivery identity is immutable' USING ERRCODE='23514';
        END IF;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER deployment_search_delivery_immutable BEFORE UPDATE OF rust_search_delivery_mode ON deployment
    FOR EACH ROW EXECUTE FUNCTION rust_search_delivery_immutable();
CREATE TRIGGER rust_search_generation_delivery_immutable BEFORE UPDATE OF delivery_kind ON rust_search_generation
    FOR EACH ROW EXECUTE FUNCTION rust_search_delivery_immutable();

CREATE FUNCTION rust_search_artifact_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'native retained artifact lifecycle is required' USING ERRCODE='23514';
END $$;
CREATE TRIGGER rust_search_artifact_immutable BEFORE UPDATE OR DELETE ON rust_search_artifact
    FOR EACH ROW EXECUTE FUNCTION rust_search_artifact_immutable();

CREATE FUNCTION rust_search_upload_scope_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'native retained upload lifecycle is required' USING ERRCODE='23514'; END IF;
    IF (NEW.generation_id,NEW.project_id,NEW.tenant_id,NEW.deployment_id,NEW.snapshot_sha256,NEW.artifact_sha256,
        NEW.artifact_bytes,NEW.documents_sha256,NEW.document_count,NEW.object_key)
       IS DISTINCT FROM
       (OLD.generation_id,OLD.project_id,OLD.tenant_id,OLD.deployment_id,OLD.snapshot_sha256,OLD.artifact_sha256,
        OLD.artifact_bytes,OLD.documents_sha256,OLD.document_count,OLD.object_key) THEN
        RAISE EXCEPTION 'native upload scope is immutable' USING ERRCODE='23514';
    END IF;
    IF EXISTS(SELECT 1 FROM rust_search_artifact WHERE generation_id=OLD.generation_id AND artifact_sha256=OLD.artifact_sha256)
       AND NEW.state IS DISTINCT FROM 'verified' THEN RAISE EXCEPTION 'attached upload must remain verified' USING ERRCODE='23514'; END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER rust_search_upload_scope_immutable BEFORE UPDATE OR DELETE ON rust_search_artifact_upload
    FOR EACH ROW EXECUTE FUNCTION rust_search_upload_scope_immutable();

CREATE FUNCTION rust_search_materialization_scope_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'native retained location evidence is immutable' USING ERRCODE='23514'; END IF;
    IF (NEW.materialization_id,NEW.replica_id,NEW.root_id,NEW.generation_id,NEW.project_id,NEW.tenant_id,NEW.artifact_sha256,NEW.created_at)
       IS DISTINCT FROM (OLD.materialization_id,OLD.replica_id,OLD.root_id,OLD.generation_id,OLD.project_id,OLD.tenant_id,OLD.artifact_sha256,OLD.created_at)
       OR (OLD.state='purged' AND NEW.state!='purged') THEN
        RAISE EXCEPTION 'native retained location scope is immutable' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER rust_search_materialization_scope_guard BEFORE UPDATE OR DELETE ON rust_search_materialization
    FOR EACH ROW EXECUTE FUNCTION rust_search_materialization_scope_guard();
CREATE TRIGGER rust_search_replica_root_immutable BEFORE UPDATE OR DELETE ON rust_search_replica_root
    FOR EACH ROW EXECUTE FUNCTION rust_search_artifact_immutable();

CREATE FUNCTION rust_search_artifact_attachment_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS(SELECT 1 FROM rust_search_generation g JOIN rust_search_artifact_upload u
          ON u.generation_id=g.generation_id AND u.artifact_sha256=NEW.artifact_sha256
        WHERE g.generation_id=NEW.generation_id AND g.delivery_kind='object'
          AND u.state='verified' AND u.artifact_bytes=NEW.artifact_bytes AND u.object_key=NEW.object_key
          AND u.documents_sha256=NEW.documents_sha256 AND u.document_count=NEW.document_count) THEN
        RAISE EXCEPTION 'artifact requires verified object upload' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER rust_search_artifact_attachment_guard BEFORE INSERT ON rust_search_artifact
    FOR EACH ROW EXECUTE FUNCTION rust_search_artifact_attachment_guard();

CREATE FUNCTION rust_search_object_ready_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.state IN ('ready','superseded') AND NEW.delivery_kind='object' THEN
        IF NOT EXISTS (
            SELECT 1 FROM rust_search_artifact a JOIN rust_search_artifact_upload u
              ON u.generation_id=a.generation_id AND u.artifact_sha256=a.artifact_sha256
            WHERE a.generation_id=NEW.generation_id AND a.project_id=NEW.project_id AND a.tenant_id=NEW.tenant_id
              AND a.deployment_id=NEW.deployment_id AND a.snapshot_sha256=NEW.snapshot_sha256
              AND a.documents_sha256=NEW.documents_sha256 AND a.document_count=NEW.document_count
              AND a.format_version=1 AND a.artifact_bytes=u.artifact_bytes AND a.object_key=u.object_key
              AND a.documents_sha256=u.documents_sha256 AND a.document_count=u.document_count AND u.state='verified'
        ) THEN RAISE EXCEPTION 'object generation requires verified artifact attachment' USING ERRCODE='23514'; END IF;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER rust_search_object_ready_guard BEFORE INSERT OR UPDATE ON rust_search_generation
    FOR EACH ROW EXECUTE FUNCTION rust_search_object_ready_guard();
COMMIT;
