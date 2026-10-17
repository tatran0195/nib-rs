#!/usr/bin/env bash
# Runs the implemented Rust vertical slices against a disposable PostgreSQL DB.
# It intentionally applies Nibleaf's real legacy migration files before the
# Rust expand migration; this is not a mock/schema imitation test.
set -euo pipefail

: "${NIBLEAF_TEST_DATABASE_URL:?Set a disposable PostgreSQL URL, e.g. postgresql://nibleaf_test:...@127.0.0.1:5432/nibleaf_rust_test}"
if [[ "$NIBLEAF_TEST_DATABASE_URL" != *"nibleaf_rust_test"* ]]; then
  echo "Refusing to reset a database whose URL does not contain nibleaf_rust_test." >&2
  exit 2
fi

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
rust_root="$root/rust"
api_pid=''
preflight_api_pid=''
worker_pid=''
metrics_bearer_token='test-metrics-token'
metrics_auth_header="authorization: Bearer $metrics_bearer_token"
# Base64 for a disposable 32-byte test key; production obtains this from its
# secret manager and rotates prior values through the bounded key ring.
invitation_signing_key='MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY='
identity_delivery_encryption_key='ZmVkY2JhOTg3NjU0MzIxMGZlZGNiYTk4NzY1NDMyMTA='
identity_delivery_key_version='2'
identity_delivery_previous_keys='1:YWJjZGVmMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODk='
search_index_root=/tmp/nibleaf-rust-search-generations
rm -rf "$search_index_root"
cleanup() {
  [[ -z "$worker_pid" ]] || kill "$worker_pid" 2>/dev/null || true
  [[ -z "$api_pid" ]] || kill "$api_pid" 2>/dev/null || true
  [[ -z "$preflight_api_pid" ]] || kill "$preflight_api_pid" 2>/dev/null || true
  wait "$worker_pid" 2>/dev/null || true
  wait "$api_pid" 2>/dev/null || true
  wait "$preflight_api_pid" 2>/dev/null || true
  rm -rf "$search_index_root"
}
trap cleanup EXIT

# The database itself must be pre-created by the caller with a restricted test
# role. Everything inside public is disposable.
psql "$NIBLEAF_TEST_DATABASE_URL" -v ON_ERROR_STOP=1 <<'SQL'
DROP SCHEMA public CASCADE;
CREATE SCHEMA public;
GRANT ALL ON SCHEMA public TO PUBLIC;
SQL
for migration in "$root"/packages/database/prisma/migrations/*/migration.sql; do
  psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 -f "$migration"
done
for migration in "$rust_root"/migrations/*.sql; do
  psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 -f "$migration"
done
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 <<'SQL'
INSERT INTO "user" (id, name, email, "emailVerified", role, banned, "createdAt", "updatedAt") VALUES
 ('test-owner', 'Owner', 'owner@example.test', TRUE, 'user', FALSE, NOW(), NOW()),
 ('test-outsider', 'Outsider', 'outsider@example.test', TRUE, 'user', FALSE, NOW(), NOW()),
 ('test-unrelated', 'Unrelated', 'unrelated@example.test', TRUE, 'user', FALSE, NOW(), NOW()),
 ('test-moderation-target', 'Moderation Target', 'moderation-target@example.test', TRUE, 'user', FALSE, NOW(), NOW()),
 ('test-support-admin-target', 'Support Admin Target', 'support-admin-target@example.test', TRUE, 'admin', FALSE, NOW(), NOW()),
 ('test-identity-unconfigured', 'Identity Unconfigured', 'identity-unconfigured@example.test', TRUE, 'user', FALSE, NOW(), NOW());
INSERT INTO session (id, "expiresAt", token, "createdAt", "updatedAt", "userId") VALUES
 ('test-owner-session', NOW() + interval '1 day', 'test-owner-token', NOW(), NOW(), 'test-owner'),
 ('test-outsider-session', NOW() + interval '1 day', 'test-outsider-token', NOW(), NOW(), 'test-outsider'),
 ('test-unrelated-session', NOW() + interval '1 day', 'test-unrelated-token', NOW(), NOW(), 'test-unrelated'),
 ('test-moderation-target-session', NOW() + interval '1 day', 'test-moderation-target-token', NOW(), NOW(), 'test-moderation-target'),
 ('test-expired-session', NOW() - interval '1 day', 'test-expired-token', NOW(), NOW(), 'test-owner');
SQL

(
  cd "$rust_root"
  cargo build --locked --workspace --bins
  # The native registry proof creates its own isolated project/deployments in
  # this disposable DB. It demonstrates stale-lease fencing and atomic active
  # generation promotion.
  NIBLEAF_SEARCH_REGISTRY_E2E=1 NIBLEAF_TEST_DATABASE_URL="$NIBLEAF_TEST_DATABASE_URL" \
    cargo test --locked -p nibleaf-search --test postgres_generation_registry \
      -- --exact reclaimed_build_cannot_promote_over_the_active_generation
  # Public reader admission is deliberately read-only and not an HTTP search
  # route yet. Exercise all legacy access modes, page grants, revocation,
  # member override, takedown concealment, and project-bound cookie scope.
  NIBLEAF_PUBLIC_READER_ADMISSION_E2E=1 NIBLEAF_TEST_DATABASE_URL="$NIBLEAF_TEST_DATABASE_URL" \
    cargo test --locked -p nibleaf-search --test postgres_public_reader_admission \
      -- --exact public_workspace_and_reader_admission_stays_project_scoped_and_fail_closed
  # Non-HTTP public composition: latest READY (including the promotion gap),
  # published ceilings intersected with live bounds, immutable bindings,
  # same-MVCC-snapshot concurrency and fresh workspace/reader admission.
  NIBLEAF_PUBLISHED_SEARCH_E2E=1 NIBLEAF_TEST_DATABASE_URL="$NIBLEAF_TEST_DATABASE_URL" \
    cargo test --locked -p nibleaf-search --test postgres_public_reader_admission \
      -- --exact published_search_uses_one_ready_snapshot_with_live_restrictions
  # Supervised query execution: SQL transfer/validation limits, admission
  # before DB work, pool-wait deadlines and no abandoned server query on drop.
  NIBLEAF_SEARCH_QUERY_EXECUTION_E2E=1 NIBLEAF_TEST_DATABASE_URL="$NIBLEAF_TEST_DATABASE_URL" \
    cargo test --locked -p nibleaf-search --test postgres_public_reader_admission \
      -- --exact bounded_query_service_owns_database_and_cpu_lifecycles
  NIBLEAF_SEARCH_LOAD_E2E=1 NIBLEAF_TEST_DATABASE_URL="$NIBLEAF_TEST_DATABASE_URL" \
    cargo test --locked -p nibleaf-search --test postgres_public_reader_admission \
      -- --exact public_query_load_and_offline_recovery_are_repeatable --nocapture
  NIBLEAF_SEARCH_OBJECT_DISTRIBUTION_E2E=1 NIBLEAF_TEST_DATABASE_URL="$NIBLEAF_TEST_DATABASE_URL" \
    cargo test --locked -p nibleaf-search --test postgres_public_reader_admission \
      -- --exact object_publication_and_replica_lifecycles_are_fenced_in_postgres --nocapture
  # Publication must stay BUILDING until a durable registry row is ready, and
  # terminal search failure must fail the deployment rather than emit ready.
  NIBLEAF_PUBLICATION_SEARCH_E2E=1 NIBLEAF_TEST_DATABASE_URL="$NIBLEAF_TEST_DATABASE_URL" \
    cargo test --locked -p nibleaf-publishing --test postgres_publication_search_ordering \
      -- --exact publication_waits_for_search_promotion_and_fails_on_terminal_search_error
)
# The API must reject the pre-202610150011 audit constraint at startup instead
# of accepting traffic and failing a valid shared-workspace deletion later.
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 <<'SQL'
ALTER TABLE rust_identity_account_deletion_audit
  DROP CONSTRAINT rust_identity_account_deletion_audit_action;
ALTER TABLE rust_identity_account_deletion_audit
  ADD CONSTRAINT rust_identity_account_deletion_audit_action CHECK (
    action = 'self_deleted_without_memberships'
  );
SQL
if NIBLEAF_DATABASE_URL="$NIBLEAF_TEST_DATABASE_URL" NIBLEAF_METRICS_BEARER_TOKEN="$metrics_bearer_token" NIBLEAF_INVITATION_SIGNING_KEY="$invitation_signing_key" NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY="$identity_delivery_encryption_key" NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY_VERSION="$identity_delivery_key_version" NIBLEAF_IDENTITY_DELIVERY_PREVIOUS_KEYS="$identity_delivery_previous_keys" NIBLEAF_NATIVE_IDENTITY_ENABLED=true NIBLEAF_BIND_ADDR=127.0.0.1:4314 \
  "$rust_root/target/debug/nibleaf-api" > /tmp/nibleaf-rust-api-missing-shared-deletion-migration.log 2>&1; then
  echo 'API unexpectedly started without the shared-workspace deletion migration.' >&2
  exit 1
fi
grep -F 'required Rust platform migrations have not been applied' /tmp/nibleaf-rust-api-missing-shared-deletion-migration.log >/dev/null
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 -f "$rust_root/migrations/202610150011_identity_account_deletion_shared_memberships.sql"
# Publication now admits native-search work, so a table-only registry migration
# check is unsafe: the old positive-only count constraint would reject a valid
# navigation-only publication after HTTP admission. Prove API startup refuses
# that stale constraint, then restore the additive expansion migration.
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 <<'SQL'
-- The isolated registry/order tests above intentionally include a valid empty
-- generation. Clear only their disposable evidence before simulating the
-- historical positive-only constraint; PostgreSQL cannot add that old check to
-- a table that already contains the newly valid value.
TRUNCATE TABLE rust_search_artifact, rust_search_artifact_upload, rust_search_materialization, rust_search_replica_root, rust_search_active_generation, rust_search_generation;
ALTER TABLE rust_search_generation
  DROP CONSTRAINT rust_search_generation_document_count_check;
ALTER TABLE rust_search_generation
  ADD CONSTRAINT rust_search_generation_document_count_check
    CHECK (document_count IS NULL OR document_count > 0);
SQL
if NIBLEAF_DATABASE_URL="$NIBLEAF_TEST_DATABASE_URL" NIBLEAF_METRICS_BEARER_TOKEN="$metrics_bearer_token" NIBLEAF_INVITATION_SIGNING_KEY="$invitation_signing_key" NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY="$identity_delivery_encryption_key" NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY_VERSION="$identity_delivery_key_version" NIBLEAF_IDENTITY_DELIVERY_PREVIOUS_KEYS="$identity_delivery_previous_keys" NIBLEAF_NATIVE_IDENTITY_ENABLED=true NIBLEAF_BIND_ADDR=127.0.0.1:4314 \
  "$rust_root/target/debug/nibleaf-api" > /tmp/nibleaf-rust-api-missing-search-generation-migration.log 2>&1; then
  echo 'API unexpectedly started without the empty-generation migration.' >&2
  exit 1
fi
grep -F 'required Rust platform migrations have not been applied' /tmp/nibleaf-rust-api-missing-search-generation-migration.log >/dev/null
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 -f "$rust_root/migrations/202610160013_search_empty_generations.sql"
# Provider refusal is an HTTP admission boundary, not a best-effort worker
# outcome: an enabled native API without Postmark/SMTP must refuse a required
# email-change proof. Use a short-lived server so the normal API test process
# can separately use a non-routable provider token for state-transition tests.
env -u NIBLEAF_POSTMARK_API_KEY -u POSTMARK_API_KEY -u NIBLEAF_SMTP_URL -u SMTP_URL \
  NIBLEAF_DATABASE_URL="$NIBLEAF_TEST_DATABASE_URL" NIBLEAF_METRICS_BEARER_TOKEN="$metrics_bearer_token" NIBLEAF_INVITATION_SIGNING_KEY="$invitation_signing_key" NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY="$identity_delivery_encryption_key" NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY_VERSION="$identity_delivery_key_version" NIBLEAF_IDENTITY_DELIVERY_PREVIOUS_KEYS="$identity_delivery_previous_keys" NIBLEAF_NATIVE_IDENTITY_ENABLED=true NIBLEAF_BIND_ADDR=127.0.0.1:4313 \
  "$rust_root/target/debug/nibleaf-api" > /tmp/nibleaf-rust-api-preflight.log 2>&1 &
preflight_api_pid=$!
curl --retry 20 --retry-connrefused --retry-delay 0 --fail --silent http://127.0.0.1:4313/health/ready >/dev/null
unconfigured_challenge_id=$(psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -v ON_ERROR_STOP=1 -c "INSERT INTO rust_identity_otp_challenge (id, normalized_email, purpose, code_hash, expires_at) VALUES (gen_random_uuid(), 'identity-unconfigured@example.test', 'sign_in', digest('000111', 'sha256'), NOW() + interval '10 minutes') RETURNING id")
unconfigured_verify=$(curl --fail --silent -D /tmp/nibleaf-identity-unconfigured-verify.headers -X POST http://127.0.0.1:4313/v1/identity/otp/verify \
  -H 'origin: http://localhost:4310' -H 'content-type: application/json' \
  --data '{"challenge_id":"'"$unconfigured_challenge_id"'","code":"000111"}')
unconfigured_csrf=$(printf '%s' "$unconfigured_verify" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["csrf_token"])')
unconfigured_cookie=$(grep -Fi 'set-cookie: nibleaf_identity=' /tmp/nibleaf-identity-unconfigured-verify.headers | sed -E 's/^[Ss]et-[Cc]ookie: nibleaf_identity=([^;]+).*/\1/' | tr -d '\r')
response=$(curl --silent -o /tmp/nibleaf-email-change-provider-refused.json -w '%{http_code}' -X POST http://127.0.0.1:4313/v1/identity/email-change/request \
  -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$unconfigured_cookie" -H "x-csrf-token: $unconfigured_csrf" -H 'content-type: application/json' \
  --data '{"new_email":"provider-refusal@example.test"}')
test "$response" = 503
kill "$preflight_api_pid"
wait "$preflight_api_pid" 2>/dev/null || true
preflight_api_pid=''
# With the rollout gate off, the identity router is absent and the native cookie
# is not admitted as a feature actor.
NIBLEAF_DATABASE_URL="$NIBLEAF_TEST_DATABASE_URL" NIBLEAF_METRICS_BEARER_TOKEN="$metrics_bearer_token" NIBLEAF_INVITATION_SIGNING_KEY="$invitation_signing_key" NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY="$identity_delivery_encryption_key" NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY_VERSION="$identity_delivery_key_version" NIBLEAF_IDENTITY_DELIVERY_PREVIOUS_KEYS="$identity_delivery_previous_keys" NIBLEAF_NATIVE_IDENTITY_ENABLED=false NIBLEAF_BIND_ADDR=127.0.0.1:4313 \
  "$rust_root/target/debug/nibleaf-api" > /tmp/nibleaf-rust-api-disabled.log 2>&1 &
preflight_api_pid=$!
curl --retry 20 --retry-connrefused --retry-delay 0 --fail --silent http://127.0.0.1:4313/health/ready >/dev/null
response=$(curl --silent -o /tmp/nibleaf-identity-disabled-route.json -w '%{http_code}' http://127.0.0.1:4313/v1/identity/session -H "cookie: nibleaf_identity=$unconfigured_cookie")
test "$response" = 404
response=$(curl --silent -o /tmp/nibleaf-identity-disabled-admission.json -w '%{http_code}' http://127.0.0.1:4313/v1/projects -H "cookie: nibleaf_identity=$unconfigured_cookie")
test "$response" = 401
kill "$preflight_api_pid"
wait "$preflight_api_pid" 2>/dev/null || true
preflight_api_pid=''

NIBLEAF_DATABASE_URL="$NIBLEAF_TEST_DATABASE_URL" NIBLEAF_METRICS_BEARER_TOKEN="$metrics_bearer_token" NIBLEAF_INVITATION_SIGNING_KEY="$invitation_signing_key" NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY="$identity_delivery_encryption_key" NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY_VERSION="$identity_delivery_key_version" NIBLEAF_IDENTITY_DELIVERY_PREVIOUS_KEYS="$identity_delivery_previous_keys" RUST_LOG='info,nibleaf_api=debug,tower_http=debug' NIBLEAF_POSTMARK_API_KEY=integration-api-only-provider NIBLEAF_NATIVE_IDENTITY_ENABLED=true NIBLEAF_SEARCH_INDEX_ROOT="$search_index_root" NIBLEAF_BIND_ADDR=127.0.0.1:4311 \
  "$rust_root/target/debug/nibleaf-api" > /tmp/nibleaf-rust-api.log 2>&1 &
api_pid=$!
curl --retry 20 --retry-connrefused --retry-delay 0 --fail --silent http://127.0.0.1:4311/health/ready >/dev/null
response=$(curl --silent -D /tmp/nibleaf-metrics-unauthorized.headers -o /tmp/nibleaf-metrics-unauthorized.json -w '%{http_code}' \
  http://127.0.0.1:4311/metrics)
test "$response" = 401
grep -Fi 'www-authenticate: Bearer' /tmp/nibleaf-metrics-unauthorized.headers >/dev/null
grep -Fi 'content-type: application/problem+json' /tmp/nibleaf-metrics-unauthorized.headers >/dev/null
python3 - <<'PY'
import json
with open('/tmp/nibleaf-metrics-unauthorized.json', encoding='utf-8') as response:
    problem = json.load(response)
assert problem['status'] == 401
assert problem['type'].endswith('/unauthenticated')
PY
response=$(curl --silent -o /tmp/nibleaf-metrics-invalid.json -w '%{http_code}' \
  -H 'authorization: Bearer wrong-metrics-token' http://127.0.0.1:4311/metrics)
test "$response" = 401
curl --fail --silent -H "$metrics_auth_header" http://127.0.0.1:4311/metrics | grep -F 'nibleaf_http_requests_total' >/dev/null

# The transition adapter fails closed for expired sessions and ambiguous cookie
# credentials; no malformed header may fall back to another credential source.
response=$(curl --silent -D /tmp/nibleaf-expired-session.headers -o /tmp/nibleaf-expired-session.json -w '%{http_code}' \
  http://127.0.0.1:4311/v1/projects -H 'authorization: Bearer test-expired-token')
test "$response" = 401
grep -Fi 'www-authenticate: Bearer' /tmp/nibleaf-expired-session.headers >/dev/null
response=$(curl --silent -o /tmp/nibleaf-ambiguous-cookie.json -w '%{http_code}' \
  http://127.0.0.1:4311/v1/projects -H 'cookie: nibleaf_session=test-owner-token; nibleaf_session=wrong-token')
test "$response" = 401

# Framework-owned routing and admission rejections must not bypass the v1
# Problem Details contract. The normalization layer must preserve request IDs
# and the protocol-required Allow header for a method mismatch.
response=$(curl --silent -D /tmp/nibleaf-route-not-found.headers -o /tmp/nibleaf-route-not-found.json -w '%{http_code}' \
  http://127.0.0.1:4311/v1/not-implemented)
test "$response" = 404
grep -Fi 'content-type: application/problem+json' /tmp/nibleaf-route-not-found.headers >/dev/null
grep -Fi 'x-request-id:' /tmp/nibleaf-route-not-found.headers >/dev/null
response=$(curl --silent -D /tmp/nibleaf-method-not-allowed.headers -o /tmp/nibleaf-method-not-allowed.json -w '%{http_code}' \
  -X PUT http://127.0.0.1:4311/v1/projects)
test "$response" = 405
grep -Fi 'content-type: application/problem+json' /tmp/nibleaf-method-not-allowed.headers >/dev/null
grep -Fi 'allow: POST' /tmp/nibleaf-method-not-allowed.headers >/dev/null
python3 -c 'import sys; sys.stdout.buffer.write(b"x" * (2 * 1024 * 1024 + 1))' > /tmp/nibleaf-payload-too-large.body
response=$(curl --silent -D /tmp/nibleaf-payload-too-large.headers -o /tmp/nibleaf-payload-too-large.json -w '%{http_code}' \
  -X POST http://127.0.0.1:4311/v1/projects -H 'content-type: application/json' --data-binary @/tmp/nibleaf-payload-too-large.body)
test "$response" = 413
grep -Fi 'content-type: application/problem+json' /tmp/nibleaf-payload-too-large.headers >/dev/null
python3 - <<'PY'
import json
for path, status, problem_type in (
    ('/tmp/nibleaf-route-not-found.json', 404, 'not-found'),
    ('/tmp/nibleaf-method-not-allowed.json', 405, 'method-not-allowed'),
    ('/tmp/nibleaf-payload-too-large.json', 413, 'payload-too-large'),
):
    with open(path, encoding='utf-8') as response:
        problem = json.load(response)
    assert problem['status'] == status
    assert problem['type'].endswith(f'/{problem_type}')
PY

create=$(curl --fail --silent -X POST http://127.0.0.1:4311/v1/projects \
  -H 'authorization: Bearer test-owner-token' -H 'content-type: application/json' \
  --data '{"name":"PostgreSQL integration project"}')
project_id=$(printf '%s' "$create" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["id"])')
cookie_projects=$(curl --fail --silent http://127.0.0.1:4311/v1/projects -H 'cookie: nibleaf_session=test-owner-token')
printf '%s' "$cookie_projects" | python3 -c "import json,sys; assert any(project['id'] == '$project_id' for project in json.load(sys.stdin)['data'])"
# ADR-0007's additive foundation is deliberately inert until the complete
# invitation lifecycle ships, but its tenant/role/idempotency constraints must
# be exercised against the real migrated schema now.
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 <<SQL
INSERT INTO member_invitation (
  id, project_id, organization_id, recipient_email, requested_role, delivery_mode,
  expires_at, inviter_id, idempotency_key_hash, request_fingerprint
)
SELECT gen_random_uuid(), p.id, p."organizationId", 'invitee@example.test', 'member', 'manual',
       NOW() + interval '7 days', 'test-owner', digest('manual-invite-one', 'sha256'), digest('request-one', 'sha256')
FROM project AS p WHERE p.id = '$project_id';
SQL
if psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 2>/tmp/nibleaf-duplicate-invitation.err <<SQL
INSERT INTO member_invitation (
  id, project_id, organization_id, recipient_email, requested_role, delivery_mode,
  expires_at, inviter_id, idempotency_key_hash, request_fingerprint
)
SELECT gen_random_uuid(), p.id, p."organizationId", 'invitee@example.test', 'admin', 'manual',
       NOW() + interval '7 days', 'test-owner', digest('manual-invite-two', 'sha256'), digest('request-two', 'sha256')
FROM project AS p WHERE p.id = '$project_id';
SQL
then
  echo 'expected duplicate pending invitation recipient constraint failure' >&2
  exit 1
fi
grep -F 'member_invitation_one_pending_recipient_key' /tmp/nibleaf-duplicate-invitation.err >/dev/null
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM member_invitation WHERE project_id = '$project_id' AND recipient_email = 'invitee@example.test'" | grep -Fx 1
event_id=$(psql "$NIBLEAF_TEST_DATABASE_URL" -At -v ON_ERROR_STOP=1 -c "SELECT id FROM outbox_event WHERE event_type='project.initialized' AND aggregate_id='$project_id'")
# The database-backed fence test claims this event twice with the *same* worker
# ID, verifies a fresh claim token, then restores the explicit crash state below.
NIBLEAF_LEASE_FENCE_EVENT_ID="$event_id" cargo test --manifest-path "$rust_root/Cargo.toml" --locked --package nibleaf-outbox --test postgres_lease_fencing
# Native Rust identity state remains additive while Better Auth is authoritative,
# but its session/CSRF/OTP lifecycle and atomic encrypted delivery intent must
# already prove their PostgreSQL fences.
NIBLEAF_IDENTITY_E2E=1 cargo test --manifest-path "$rust_root/Cargo.toml" --locked --package nibleaf-identity --test postgres_identity_lifecycle

# The worker deliberately runs with no email provider. This proves durable
# dead-letter evidence and prevents this disposable harness from ever using
# caller-supplied real provider credentials.
env -u NIBLEAF_POSTMARK_API_KEY -u POSTMARK_API_KEY -u NIBLEAF_SMTP_URL -u SMTP_URL \
  NIBLEAF_DATABASE_URL="$NIBLEAF_TEST_DATABASE_URL" NIBLEAF_METRICS_BEARER_TOKEN="$metrics_bearer_token" NIBLEAF_INVITATION_SIGNING_KEY="$invitation_signing_key" NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY="$identity_delivery_encryption_key" NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY_VERSION="$identity_delivery_key_version" NIBLEAF_IDENTITY_DELIVERY_PREVIOUS_KEYS="$identity_delivery_previous_keys" NIBLEAF_SEARCH_INDEX_ROOT="$search_index_root" NIBLEAF_WORKER_BIND_ADDR=127.0.0.1:4312 NIBLEAF_WORKER_CONCURRENCY=2 NIBLEAF_SUPPORT_EXPIRY_RECONCILIATION_INTERVAL_SECONDS=5 \
  "$rust_root/target/debug/nibleaf-worker" > /tmp/nibleaf-rust-worker.log 2>&1 &
worker_pid=$!
curl --retry 20 --retry-connrefused --retry-delay 0 --fail --silent http://127.0.0.1:4312/health/ready >/dev/null
sleep 1
worker_metrics=$(curl --fail --silent -H "$metrics_auth_header" http://127.0.0.1:4312/metrics)
printf '%s' "$worker_metrics" | grep -F 'nibleaf_outbox_claimed_total' >/dev/null
printf '%s' "$worker_metrics" | grep -F 'nibleaf_outbox_reclaimed_total' >/dev/null
printf '%s' "$worker_metrics" | grep -F 'nibleaf_outbox_delivery_total' >/dev/null
printf '%s' "$worker_metrics" | grep -E 'nibleaf_outbox_events\{[^}]*state="pending"' >/dev/null
printf '%s' "$worker_metrics" | grep -E 'nibleaf_outbox_snapshot_up\{[^}]*service="nibleaf-worker"[^}]*\} 1' >/dev/null
printf '%s' "$worker_metrics" | grep -E 'nibleaf_database_pool_connections\{[^}]*state="total"' >/dev/null
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT state || ':' || attempts FROM outbox_event WHERE event_type='project.initialized'" | grep -Fx 'delivered:2'
# New native accounts receive a durable, at-least-once provisioning intent. It
# creates the rich Better-Auth-equivalent starter shape and requests a separate
# durable automatic first publication without holding a worker transaction over
# unrelated external calls.
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT state FROM outbox_event WHERE event_type='identity.account.provision_requested'" | grep -Fx 'delivered'
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM member AS m INNER JOIN \"user\" AS u ON u.id=m.\"userId\" WHERE u.email='identity-new-account@example.test' AND m.role='owner'" | grep -Fx 1
starter_project_id=$(psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -c "SELECT p.id FROM project AS p INNER JOIN member AS m ON m.\"organizationId\"=p.\"organizationId\" INNER JOIN \"user\" AS u ON u.id=m.\"userId\" WHERE u.email='identity-new-account@example.test'")
test -n "$starter_project_id"
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT name || ':' || description FROM project WHERE id='$starter_project_id'" | grep -Fx 'Documentation:Your first documentation site.'
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM page WHERE \"projectId\"='$starter_project_id'" | grep -Fx 7
psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -c "SELECT path || ':' || encode(digest(content,'sha256'),'hex') FROM page WHERE \"projectId\"='$starter_project_id' ORDER BY path" > /tmp/nibleaf-native-starter-content.sha256
cat > /tmp/nibleaf-native-starter-content.expected <<'EOF'
api-reference:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
api-reference/authentication:cb9a87bb55e94226fe0ba27e77128d0fd92cc15c49db41d5ab6fe8f22ea40191
api-reference/create-a-widget:6e57f678a1e6d6c2d1b0e296c797198bef15d1fc83d76f609ee18a0849e5c6a8
components-tour:0744b57807b60cb945d1f123c7249460ec14fbf86e085a8079d4afd89b0bea08
faq:d3ae7c48ae1c06f6068c2adb040074630a0d75f070936f24b93a2a12b81ef228
getting-started:5c5421e5a806863988214e1a7622f3d0af08d74d92f1314c38d1aa487bebc5b1
installation:4d93468cd0dac9003cccd4e9e5ebd47f8e1e6d787ce62a729af20392e3e73616
EOF
diff -u /tmp/nibleaf-native-starter-content.expected /tmp/nibleaf-native-starter-content.sha256
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM page WHERE \"projectId\"='$starter_project_id' AND kind='GROUP'::\"PageKind\" AND path='api-reference'" | grep -Fx 1
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM page WHERE \"projectId\"='$starter_project_id' AND path IN ('api-reference/authentication','api-reference/create-a-widget')" | grep -Fx 2
starter_publication_event_id=$(psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -c "SELECT id FROM outbox_event WHERE event_type='starter.publication.requested' AND aggregate_id='$starter_project_id'")
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT state FROM outbox_event WHERE id='$starter_publication_event_id'" | grep -Fx delivered
for _ in $(seq 1 20); do
  starter_publication=$(psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -c "SELECT status::text || ':' || \"pagesCount\" || ':' || \"commitMessage\" FROM deployment WHERE \"projectId\"='$starter_project_id'")
  [[ "$starter_publication" == 'READY:7:Publish starter site' ]] && break
  sleep 1
done
printf '%s\n' "$starter_publication" | grep -Fx 'READY:7:Publish starter site'
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT state FROM rust_search_generation WHERE project_id='$starter_project_id'" | grep -Fx ready
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM rust_search_active_generation WHERE project_id='$starter_project_id'" | grep -Fx 1
# Re-delivery of the durable starter-publication intent must not create a
# second "first" deployment after the project is already live.
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 -c "UPDATE outbox_event SET state='pending', lease_token=NULL, leased_until=NULL, worker_id=NULL, delivered_at=NULL WHERE id='$starter_publication_event_id'"
sleep 1
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT state || ':' || attempts FROM outbox_event WHERE id='$starter_publication_event_id'" | grep -E '^delivered:[2-9][0-9]*$'
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM deployment WHERE \"projectId\"='$starter_project_id'" | grep -Fx 1
# A tenant-scoped starter request must not be able to publish another tenant's
# project, even with an otherwise well-formed aggregate and account payload.
starter_owner_id=$(psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -c "SELECT \"userId\" FROM member WHERE \"organizationId\"=(SELECT \"organizationId\" FROM project WHERE id='$starter_project_id') AND role='owner'")
foreign_tenant_id=$(psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -c "SELECT \"organizationId\" FROM project WHERE id='$project_id'")
foreign_starter_event_id=$(psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -v ON_ERROR_STOP=1 -c "INSERT INTO outbox_event (id,tenant_id,aggregate_type,aggregate_id,event_type,schema_version,payload,correlation_id) VALUES (gen_random_uuid(), '$foreign_tenant_id', 'project', '$starter_project_id', 'starter.publication.requested', 1, jsonb_build_object('project_id','$starter_project_id','created_by_user_id','$starter_owner_id'), gen_random_uuid()) RETURNING id")
sleep 1
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT state FROM outbox_event WHERE id='$foreign_starter_event_id'" | grep -Fx dead_letter
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM deployment WHERE \"projectId\"='$starter_project_id'" | grep -Fx 1

# Native OTP request now returns a browser-usable opaque continuation without
# weakening anti-enumeration. Verify must issue only its distinct host-only
# cookie, return the CSRF secret once, and admit that cookie on its own session
# endpoint. The harness changes only the disposable challenge hash to a known
# code; it never reads an OTP email payload or contacts a provider.
identity_request=$(curl --fail --silent -D /tmp/nibleaf-identity-request.headers -X POST http://127.0.0.1:4311/v1/identity/otp/request \
  -H 'origin: http://localhost:4310' -H 'content-type: application/json' \
  --data '{"email":"identity-http@example.test"}')
identity_challenge_id=$(printf '%s' "$identity_request" | python3 -c 'import json,sys,uuid; value=json.load(sys.stdin)["data"]["challenge_id"]; uuid.UUID(value); print(value)')
grep -Fi 'cache-control: no-store' /tmp/nibleaf-identity-request.headers >/dev/null
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 -c "UPDATE rust_identity_otp_challenge SET code_hash=digest('123456', 'sha256') WHERE id='$identity_challenge_id'"
identity_verify=$(curl --fail --silent -D /tmp/nibleaf-identity-verify.headers -X POST http://127.0.0.1:4311/v1/identity/otp/verify \
  -H 'origin: http://localhost:4310' -H 'content-type: application/json' \
  --data '{"challenge_id":"'"$identity_challenge_id"'","code":"123456","name":"Identity HTTP"}')
identity_csrf=$(printf '%s' "$identity_verify" | python3 -c 'import json,sys; body=json.load(sys.stdin)["data"]; assert body["csrf_token"] and body["expires_at"]; print(body["csrf_token"])')
identity_cookie=$(grep -Fi 'set-cookie: nibleaf_identity=' /tmp/nibleaf-identity-verify.headers | sed -E 's/^[Ss]et-[Cc]ookie: nibleaf_identity=([^;]+).*/\1/' | tr -d '\r')
test -n "$identity_cookie"
grep -Fi 'httponly' /tmp/nibleaf-identity-verify.headers >/dev/null
grep -Fi 'cache-control: no-store' /tmp/nibleaf-identity-verify.headers >/dev/null
# A new tab can recover an unusable in-memory CSRF secret only with the valid
# native cookie and an explicit trusted Origin. Recovery rotates the stored
# secret, so the login-time value immediately stops authorizing mutations.
response=$(curl --silent -o /tmp/nibleaf-identity-csrf-missing-origin.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/csrf -H "cookie: nibleaf_identity=$identity_cookie")
test "$response" = 403
identity_original_csrf=$identity_csrf
identity_csrf_recovery=$(curl --fail --silent -D /tmp/nibleaf-identity-csrf-recovery.headers -X POST http://127.0.0.1:4311/v1/identity/csrf -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie")
identity_csrf=$(printf '%s' "$identity_csrf_recovery" | python3 -c 'import json,sys; value=json.load(sys.stdin)["data"]["csrf_token"]; assert value; print(value)')
test "$identity_csrf" != "$identity_original_csrf"
grep -Fi 'cache-control: no-store' /tmp/nibleaf-identity-csrf-recovery.headers >/dev/null
# Malformed pre-auth input receives the same UUID-shaped request response, but
# the fake handle cannot verify or create a session.
invalid_otp_request=$(curl --fail --silent -X POST http://127.0.0.1:4311/v1/identity/otp/request -H 'origin: http://localhost:4310' -H 'content-type: application/json' --data '{"email":"not-an-email"}')
invalid_otp_challenge=$(printf '%s' "$invalid_otp_request" | python3 -c 'import json,sys,uuid; value=json.load(sys.stdin)["data"]["challenge_id"]; uuid.UUID(value); print(value)')
response=$(curl --silent -o /tmp/nibleaf-identity-fake-otp-verify.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/otp/verify -H 'origin: http://localhost:4310' -H 'content-type: application/json' --data "{\"challenge_id\":\"$invalid_otp_challenge\",\"code\":\"123456\"}")
test "$response" = 401
curl --fail --silent http://127.0.0.1:4311/v1/identity/session -H "cookie: nibleaf_identity=$identity_cookie" | python3 -c 'import json,sys; assert json.load(sys.stdin)["data"]["user_id"]'
# Existing Rust feature routes accept the injected native actor, while their
# unsafe forms receive API-wide CSRF/origin admission before feature authz.
response=$(curl --silent -o /tmp/nibleaf-native-feature-missing-csrf.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/projects/not-a-project/member-invitations -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H 'content-type: application/json' --data '{}')
test "$response" = 401
response=$(curl --silent -o /tmp/nibleaf-native-feature-stale-csrf.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/projects/not-a-project/member-invitations -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_original_csrf" -H 'content-type: application/json' --data '{}')
test "$response" = 401
response=$(curl --silent -o /tmp/nibleaf-native-feature-csrf.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/projects/not-a-project/member-invitations -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf" -H 'content-type: application/json' --data '{}')
test "$response" = 422

# Native moderation is intentionally platform-scoped, not tenant-scoped. It
# admits only a live native platform administrator, revokes both native and
# transitional sessions on suspension, and appends an immutable audit fact for
# each state-changing or idempotent operator request.
response=$(curl --silent -o /tmp/nibleaf-moderation-user-forbidden.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/admin/users/test-moderation-target/suspend -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf")
test "$response" = 403
response=$(curl --silent -o /tmp/nibleaf-moderation-project-forbidden.json -w '%{http_code}' -X POST "http://127.0.0.1:4311/v1/identity/admin/projects/$project_id/takedown" -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf" -H 'content-type: application/json' --data '{"reason":"pre-promotion"}')
test "$response" = 403
identity_user_id=$(psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -v ON_ERROR_STOP=1 -c "SELECT id FROM \"user\" WHERE email='identity-http@example.test'")
test -n "$identity_user_id"
profile_user_id=$(psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -v ON_ERROR_STOP=1 -c "SELECT id FROM \"user\" WHERE email='identity-unconfigured@example.test'")
test -n "$profile_user_id"
# The session read is the native self-service adapter surface: it retains the
# additive top-level ID while returning the profile that dashboard/session code
# needs. Unsafe profile changes remain Origin/CSRF-fenced by API admission.
curl --fail --silent http://127.0.0.1:4311/v1/identity/session -H "cookie: nibleaf_identity=$unconfigured_cookie" | python3 -c "import json,sys; data=json.load(sys.stdin)['data']; assert data['user_id'] == '$profile_user_id'; assert data['user'] == {'id':'$profile_user_id','name':'Identity Unconfigured','email':'identity-unconfigured@example.test','email_verified':True}; assert data['active_organization_id'] is None"
response=$(curl --silent -o /tmp/nibleaf-profile-missing-csrf.json -w '%{http_code}' -X PATCH http://127.0.0.1:4311/v1/identity/profile -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$unconfigured_cookie" -H 'content-type: application/json' --data '{"name":"must not persist"}')
test "$response" = 401
profile_update=$(curl --fail --silent -D /tmp/nibleaf-profile-update.headers -X PATCH http://127.0.0.1:4311/v1/identity/profile -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$unconfigured_cookie" -H "x-csrf-token: $unconfigured_csrf" -H 'content-type: application/json' --data '{"name":"  Native Profile  "}')
grep -Fi 'cache-control: no-store' /tmp/nibleaf-profile-update.headers >/dev/null
printf '%s' "$profile_update" | python3 -c "import json,sys; data=json.load(sys.stdin)['data']; assert data['user']['id'] == '$profile_user_id' and data['user']['name'] == 'Native Profile' and data['active_organization_id'] is None"
response=$(curl --silent -o /tmp/nibleaf-profile-invalid-name.json -w '%{http_code}' -X PATCH http://127.0.0.1:4311/v1/identity/profile -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$unconfigured_cookie" -H "x-csrf-token: $unconfigured_csrf" -H 'content-type: application/json' --data '{"name":"\u0000"}')
test "$response" = 422
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 <<SQL
INSERT INTO organization (id, name, slug) VALUES
 ('native-profile-organization-one', 'Native Profile One', 'native-profile-one'),
 ('native-profile-organization-two', 'Native Profile Two', 'native-profile-two'),
 ('native-profile-organization-outsider', 'Native Profile Outsider', 'native-profile-outsider');
INSERT INTO member (id, "organizationId", "userId", role) VALUES
 ('native-profile-member-one', 'native-profile-organization-one', '$profile_user_id', 'owner'),
 ('native-profile-member-two', 'native-profile-organization-two', '$profile_user_id', 'member');
SQL
# A session created before workspace realization still resolves the first live
# membership deterministically. A selected workspace must be a current
# membership, and persists only as presentation context.
curl --fail --silent http://127.0.0.1:4311/v1/identity/session -H "cookie: nibleaf_identity=$unconfigured_cookie" | python3 -c "import json,sys; assert json.load(sys.stdin)['data']['active_organization_id'] == 'native-profile-organization-one'"
response=$(curl --silent -o /tmp/nibleaf-active-organization-outsider.json -w '%{http_code}' -X PUT http://127.0.0.1:4311/v1/identity/session/active-organization -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$unconfigured_cookie" -H "x-csrf-token: $unconfigured_csrf" -H 'content-type: application/json' --data '{"organization_id":"native-profile-organization-outsider"}')
test "$response" = 422
active_organization=$(curl --fail --silent -X PUT http://127.0.0.1:4311/v1/identity/session/active-organization -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$unconfigured_cookie" -H "x-csrf-token: $unconfigured_csrf" -H 'content-type: application/json' --data '{"organization_id":"native-profile-organization-two"}')
printf '%s' "$active_organization" | python3 -c "import json,sys; data=json.load(sys.stdin)['data']; assert data['user']['id'] == '$profile_user_id' and data['active_organization_id'] == 'native-profile-organization-two'"
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT active_organization_id FROM rust_identity_session WHERE user_id='$profile_user_id' AND revoked_at IS NULL" | grep -Fx 'native-profile-organization-two'
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 -c "UPDATE \"user\" SET role='admin' WHERE id='$identity_user_id'"
response=$(curl --silent -o /tmp/nibleaf-moderation-admin-protected.json -w '%{http_code}' -X POST "http://127.0.0.1:4311/v1/identity/admin/users/$identity_user_id/suspend" -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf")
test "$response" = 409
moderation_target_challenge=$(psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -v ON_ERROR_STOP=1 -c "INSERT INTO rust_identity_otp_challenge (id, normalized_email, purpose, code_hash, expires_at) VALUES (gen_random_uuid(), 'moderation-target@example.test', 'sign_in', digest('778899', 'sha256'), NOW() + interval '10 minutes') RETURNING id")
curl --fail --silent -D /tmp/nibleaf-moderation-target-verify.headers -X POST http://127.0.0.1:4311/v1/identity/otp/verify -H 'origin: http://localhost:4310' -H 'content-type: application/json' --data '{"challenge_id":"'"$moderation_target_challenge"'","code":"778899"}' >/dev/null
moderation_target_cookie=$(grep -Fi 'set-cookie: nibleaf_identity=' /tmp/nibleaf-moderation-target-verify.headers | sed -E 's/^[Ss]et-[Cc]ookie: nibleaf_identity=([^;]+).*/\1/' | tr -d '\r')
test -n "$moderation_target_cookie"
suspension=$(curl --fail --silent -X POST http://127.0.0.1:4311/v1/identity/admin/users/test-moderation-target/suspend -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf")
printf '%s' "$suspension" | python3 -c 'import json,sys; assert json.load(sys.stdin)["data"]["changed"] is True'
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT (\"suspendedAt\" IS NOT NULL)::text || ':' || banned::text || ':' || \"banReason\" FROM \"user\" WHERE id='test-moderation-target'" | grep -Fx 'true:true:Suspended by a Nibleaf operator'
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM session WHERE \"userId\"='test-moderation-target'" | grep -Fx 0
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT action || ':' || changed::text FROM rust_identity_moderation_audit WHERE actor_user_id='$identity_user_id' AND target_user_id='test-moderation-target' ORDER BY created_at DESC LIMIT 1" | grep -Fx 'user_suspended:true'
response=$(curl --silent -o /tmp/nibleaf-moderation-revoked-legacy-session.json -w '%{http_code}' http://127.0.0.1:4311/v1/projects -H 'authorization: Bearer test-moderation-target-token')
test "$response" = 401
response=$(curl --silent -o /tmp/nibleaf-moderation-revoked-native-session.json -w '%{http_code}' http://127.0.0.1:4311/v1/identity/session -H "cookie: nibleaf_identity=$moderation_target_cookie")
test "$response" = 401
resuspension=$(curl --fail --silent -X POST http://127.0.0.1:4311/v1/identity/admin/users/test-moderation-target/suspend -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf")
printf '%s' "$resuspension" | python3 -c 'import json,sys; assert json.load(sys.stdin)["data"]["changed"] is False'
restored_user=$(curl --fail --silent -X POST http://127.0.0.1:4311/v1/identity/admin/users/test-moderation-target/unsuspend -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf")
printf '%s' "$restored_user" | python3 -c 'import json,sys; assert json.load(sys.stdin)["data"]["changed"] is True'
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT (\"suspendedAt\" IS NULL)::text || ':' || banned::text || ':' || count(s.id) FROM \"user\" AS u LEFT JOIN session AS s ON s.\"userId\"=u.id WHERE u.id='test-moderation-target' GROUP BY u.\"suspendedAt\", u.banned" | grep -Fx 'true:false:0'
response=$(curl --silent -o /tmp/nibleaf-moderation-invalid-reason.json -w '%{http_code}' -X POST "http://127.0.0.1:4311/v1/identity/admin/projects/$project_id/takedown" -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf" -H 'content-type: application/json' --data '{"reason":"line one\nline two"}')
test "$response" = 422
takedown=$(curl --fail --silent -X POST "http://127.0.0.1:4311/v1/identity/admin/projects/$project_id/takedown" -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf" -H 'content-type: application/json' --data '{"reason":"Terms violation"}')
printf '%s' "$takedown" | python3 -c 'import json,sys; assert json.load(sys.stdin)["data"]["changed"] is True'
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT (\"takedownAt\" IS NOT NULL)::text || ':' || \"takedownReason\" FROM project WHERE id='$project_id'" | grep -Fx 'true:Terms violation'
response=$(curl --silent -o /tmp/nibleaf-moderation-taken-down-publish.json -w '%{http_code}' -X POST "http://127.0.0.1:4311/v1/projects/$project_id/publications" -H 'authorization: Bearer test-owner-token' -H 'content-type: application/json' --data '{"commit_message":"must not publish while taken down"}')
test "$response" = 403
restored_project=$(curl --fail --silent -X POST "http://127.0.0.1:4311/v1/identity/admin/projects/$project_id/restore" -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf")
printf '%s' "$restored_project" | python3 -c 'import json,sys; assert json.load(sys.stdin)["data"]["changed"] is True'
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT (\"takedownAt\" IS NULL)::text || ':' || COALESCE(\"takedownReason\", '') FROM project WHERE id='$project_id'" | grep -Fx 'true:'
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT string_agg(action || ':' || changed::text, ',' ORDER BY created_at) FROM rust_identity_moderation_audit WHERE actor_user_id='$identity_user_id'" | grep -Fx 'user_suspended:true,user_suspended:false,user_unsuspended:true,project_taken_down:true,project_restored:true'
if psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 -c "UPDATE rust_identity_moderation_audit SET changed=TRUE WHERE actor_user_id='$identity_user_id'" 2>/tmp/nibleaf-moderation-audit-mutation.err; then
  echo 'expected immutable native moderation audit trigger to reject UPDATE' >&2
  exit 1
fi
grep -F 'rust_identity_moderation_audit facts are immutable' /tmp/nibleaf-moderation-audit-mutation.err >/dev/null

# Native support access creates a separate one-hour customer session and retains
# the live operator session only in a path-scoped HttpOnly resume cookie. A
# second administrator action may revoke it; the explicit stop route still has
# to restore the operator even when the customer cookie is already unusable.
response=$(curl --silent -o /tmp/nibleaf-impersonation-self.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/admin/impersonations -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf" -H 'content-type: application/json' --data "{\"target_user_id\":\"$identity_user_id\"}")
test "$response" = 422
impersonation_start=$(curl --fail --silent -D /tmp/nibleaf-impersonation-start.headers -X POST http://127.0.0.1:4311/v1/identity/admin/impersonations -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf" -H 'content-type: application/json' --data '{"target_user_id":"test-moderation-target"}')
impersonation_id=$(printf '%s' "$impersonation_start" | python3 -c 'import json,sys; body=json.load(sys.stdin)["data"]; assert body["csrf_token"] and body["expires_at"] and body["user_id"] == "test-moderation-target"; assert "session_token" not in body and "resume_token" not in body; print(body["impersonation_id"])')
impersonation_csrf=$(printf '%s' "$impersonation_start" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["csrf_token"])')
impersonation_cookie=$(grep -Fi 'set-cookie: nibleaf_identity=' /tmp/nibleaf-impersonation-start.headers | sed -E 's/^[Ss]et-[Cc]ookie: nibleaf_identity=([^;]+).*/\1/' | tr -d '\r')
impersonation_resume_cookie=$(grep -Fi 'set-cookie: nibleaf_identity_resume=' /tmp/nibleaf-impersonation-start.headers | sed -E 's/^[Ss]et-[Cc]ookie: nibleaf_identity_resume=([^;]+).*/\1/' | tr -d '\r')
test -n "$impersonation_cookie"
test -n "$impersonation_resume_cookie"
grep -Fi 'set-cookie: nibleaf_identity_resume=' /tmp/nibleaf-impersonation-start.headers | grep -F 'Path=/v1/identity/admin/impersonations/stop; HttpOnly; SameSite=Lax' >/dev/null
curl --fail --silent http://127.0.0.1:4311/v1/identity/session -H "cookie: nibleaf_identity=$impersonation_cookie" | python3 -c "import json,sys; data=json.load(sys.stdin)['data']; assert data['user_id'] == 'test-moderation-target' and data['impersonated_by'] == '$identity_user_id'"
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM rust_identity_impersonation WHERE id='$impersonation_id' AND operator_user_id='$identity_user_id' AND target_user_id='test-moderation-target' AND terminated_at IS NULL" | grep -Fx 1
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT action FROM rust_identity_impersonation_audit WHERE impersonation_id='$impersonation_id'" | grep -Fx started
revoked_impersonation=$(curl --fail --silent -X POST "http://127.0.0.1:4311/v1/identity/admin/impersonations/$impersonation_id/revoke" -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf")
printf '%s' "$revoked_impersonation" | python3 -c 'import json,sys; assert json.load(sys.stdin)["data"]["changed"] is True'
response=$(curl --silent -o /tmp/nibleaf-impersonation-revoked-session.json -w '%{http_code}' http://127.0.0.1:4311/v1/identity/session -H "cookie: nibleaf_identity=$impersonation_cookie")
test "$response" = 401
recovered_impersonation=$(curl --fail --silent -D /tmp/nibleaf-impersonation-recover.headers -X POST http://127.0.0.1:4311/v1/identity/admin/impersonations/stop -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$impersonation_cookie; nibleaf_identity_resume=$impersonation_resume_cookie")
printf '%s' "$recovered_impersonation" | python3 -c 'import json,sys; assert json.load(sys.stdin)["data"]["changed"] is False'
grep -Fi 'set-cookie: nibleaf_identity=' /tmp/nibleaf-impersonation-recover.headers | grep -F "nibleaf_identity=$identity_cookie" >/dev/null
grep -Fi 'set-cookie: nibleaf_identity_resume=; Path=/v1/identity/admin/impersonations/stop; HttpOnly; SameSite=Lax; Max-Age=0' /tmp/nibleaf-impersonation-recover.headers >/dev/null
impersonation_start_two=$(curl --fail --silent -D /tmp/nibleaf-impersonation-start-two.headers -X POST http://127.0.0.1:4311/v1/identity/admin/impersonations -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf" -H 'content-type: application/json' --data '{"target_user_id":"test-support-admin-target"}')
impersonation_id_two=$(printf '%s' "$impersonation_start_two" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["impersonation_id"])')
impersonation_csrf_two=$(printf '%s' "$impersonation_start_two" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["csrf_token"])')
impersonation_cookie_two=$(grep -Fi 'set-cookie: nibleaf_identity=' /tmp/nibleaf-impersonation-start-two.headers | sed -E 's/^[Ss]et-[Cc]ookie: nibleaf_identity=([^;]+).*/\1/' | tr -d '\r')
impersonation_resume_cookie_two=$(grep -Fi 'set-cookie: nibleaf_identity_resume=' /tmp/nibleaf-impersonation-start-two.headers | sed -E 's/^[Ss]et-[Cc]ookie: nibleaf_identity_resume=([^;]+).*/\1/' | tr -d '\r')
response=$(curl --silent -o /tmp/nibleaf-impersonation-platform-boundary.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/admin/users/test-moderation-target/suspend -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$impersonation_cookie_two" -H "x-csrf-token: $impersonation_csrf_two")
test "$response" = 403
stopped_impersonation=$(curl --fail --silent -D /tmp/nibleaf-impersonation-stop.headers -X POST http://127.0.0.1:4311/v1/identity/admin/impersonations/stop -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$impersonation_cookie_two; nibleaf_identity_resume=$impersonation_resume_cookie_two")
printf '%s' "$stopped_impersonation" | python3 -c 'import json,sys; assert json.load(sys.stdin)["data"]["changed"] is True'
response=$(curl --silent -o /tmp/nibleaf-impersonation-stopped-session.json -w '%{http_code}' http://127.0.0.1:4311/v1/identity/session -H "cookie: nibleaf_identity=$impersonation_cookie_two")
test "$response" = 401
impersonation_start_three=$(curl --fail --silent -D /tmp/nibleaf-impersonation-start-three.headers -X POST http://127.0.0.1:4311/v1/identity/admin/impersonations -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf" -H 'content-type: application/json' --data '{"target_user_id":"test-moderation-target"}')
impersonation_id_three=$(printf '%s' "$impersonation_start_three" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["impersonation_id"])')
impersonation_cookie_three=$(grep -Fi 'set-cookie: nibleaf_identity=' /tmp/nibleaf-impersonation-start-three.headers | sed -E 's/^[Ss]et-[Cc]ookie: nibleaf_identity=([^;]+).*/\1/' | tr -d '\r')
impersonation_csrf_three=$(printf '%s' "$impersonation_start_three" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["csrf_token"])')
response=$(curl --silent -D /tmp/nibleaf-impersonation-signout.headers -o /tmp/nibleaf-impersonation-signout.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/sign-out -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$impersonation_cookie_three" -H "x-csrf-token: $impersonation_csrf_three")
test "$response" = 204
grep -Fi 'set-cookie: nibleaf_identity=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0' /tmp/nibleaf-impersonation-signout.headers >/dev/null
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT string_agg(action, ',' ORDER BY created_at) FROM rust_identity_impersonation_audit WHERE impersonation_id IN ('$impersonation_id','$impersonation_id_two','$impersonation_id_three')" | grep -Fx 'started,revoked,started,ended,started,ended'
# Suspending a support target is a cross-slice security transition: it must not
# merely make the target cookie fail authentication. The same moderation
# transaction terminalizes the support lifecycle with a moderator-attributed
# revoke fact, then leaves the resume path able to recover the operator.
impersonation_start_suspension=$(curl --fail --silent -D /tmp/nibleaf-impersonation-start-suspension.headers -X POST http://127.0.0.1:4311/v1/identity/admin/impersonations -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf" -H 'content-type: application/json' --data '{"target_user_id":"test-moderation-target"}')
impersonation_id_suspension=$(printf '%s' "$impersonation_start_suspension" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["impersonation_id"])')
impersonation_cookie_suspension=$(grep -Fi 'set-cookie: nibleaf_identity=' /tmp/nibleaf-impersonation-start-suspension.headers | sed -E 's/^[Ss]et-[Cc]ookie: nibleaf_identity=([^;]+).*/\1/' | tr -d '\r')
impersonation_resume_cookie_suspension=$(grep -Fi 'set-cookie: nibleaf_identity_resume=' /tmp/nibleaf-impersonation-start-suspension.headers | sed -E 's/^[Ss]et-[Cc]ookie: nibleaf_identity_resume=([^;]+).*/\1/' | tr -d '\r')
test -n "$impersonation_cookie_suspension"
test -n "$impersonation_resume_cookie_suspension"
suspension_with_support=$(curl --fail --silent -X POST http://127.0.0.1:4311/v1/identity/admin/users/test-moderation-target/suspend -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf")
printf '%s' "$suspension_with_support" | python3 -c 'import json,sys; assert json.load(sys.stdin)["data"]["changed"] is True'
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT termination_reason || ':' || terminated_by_user_id || ':' || (terminated_by_session_id IS NOT NULL)::text FROM rust_identity_impersonation WHERE id='$impersonation_id_suspension'" | grep -Fx "revoked:$identity_user_id:true"
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT action || ':' || actor_kind || ':' || actor_user_id || ':' || (actor_session_id IS NOT NULL)::text FROM rust_identity_impersonation_audit WHERE impersonation_id='$impersonation_id_suspension' ORDER BY created_at DESC LIMIT 1" | grep -Fx "revoked:user:$identity_user_id:true"
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT s.revoke_reason FROM rust_identity_session AS s INNER JOIN rust_identity_impersonation AS i ON i.impersonation_session_id=s.id WHERE i.id='$impersonation_id_suspension'" | grep -Fx moderation_suspension
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT string_agg(action, ',' ORDER BY created_at) FROM rust_identity_impersonation_audit WHERE impersonation_id='$impersonation_id_suspension'" | grep -Fx 'started,revoked'
response=$(curl --silent -o /tmp/nibleaf-impersonation-suspended-target-session.json -w '%{http_code}' http://127.0.0.1:4311/v1/identity/session -H "cookie: nibleaf_identity=$impersonation_cookie_suspension")
test "$response" = 401
recovered_suspended_target_impersonation=$(curl --fail --silent -D /tmp/nibleaf-impersonation-suspended-target-recover.headers -X POST http://127.0.0.1:4311/v1/identity/admin/impersonations/stop -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$impersonation_cookie_suspension; nibleaf_identity_resume=$impersonation_resume_cookie_suspension")
printf '%s' "$recovered_suspended_target_impersonation" | python3 -c 'import json,sys; assert json.load(sys.stdin)["data"]["changed"] is False'
grep -Fi 'set-cookie: nibleaf_identity=' /tmp/nibleaf-impersonation-suspended-target-recover.headers | grep -F "nibleaf_identity=$identity_cookie" >/dev/null
restored_support_target=$(curl --fail --silent -X POST http://127.0.0.1:4311/v1/identity/admin/users/test-moderation-target/unsuspend -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf")
printf '%s' "$restored_support_target" | python3 -c 'import json,sys; assert json.load(sys.stdin)["data"]["changed"] is True'
# The cancellation-aware worker loop claims a bounded batch of due lifecycle
# rows. Force a fourth otherwise-normal support context due, then prove the
# periodic reconciler—not an HTTP encounter—writes one system-attributed expiry
# fact and revokes the corresponding target session.
impersonation_start_four=$(curl --fail --silent -D /tmp/nibleaf-impersonation-start-four.headers -X POST http://127.0.0.1:4311/v1/identity/admin/impersonations -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf" -H 'content-type: application/json' --data '{"target_user_id":"test-moderation-target"}')
impersonation_id_four=$(printf '%s' "$impersonation_start_four" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["impersonation_id"])')
impersonation_cookie_four=$(grep -Fi 'set-cookie: nibleaf_identity=' /tmp/nibleaf-impersonation-start-four.headers | sed -E 's/^[Ss]et-[Cc]ookie: nibleaf_identity=([^;]+).*/\1/' | tr -d '\r')
impersonation_resume_cookie_four=$(grep -Fi 'set-cookie: nibleaf_identity_resume=' /tmp/nibleaf-impersonation-start-four.headers | sed -E 's/^[Ss]et-[Cc]ookie: nibleaf_identity_resume=([^;]+).*/\1/' | tr -d '\r')
test -n "$impersonation_cookie_four"
test -n "$impersonation_resume_cookie_four"
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 -c "UPDATE rust_identity_impersonation SET expires_at=started_at + interval '1 second' WHERE id='$impersonation_id_four'"
sleep 2
expiry_state=''
for _ in {1..20}; do
  expiry_state=$(psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -v ON_ERROR_STOP=1 -c "SELECT termination_reason || ':' || terminated_by_user_id || ':' || COALESCE(terminated_by_session_id::text, '') FROM rust_identity_impersonation WHERE id='$impersonation_id_four'")
  if [[ "$expiry_state" = 'expired:system:identity-impersonation-expirer:' ]]; then
    break
  fi
  sleep 1
done
test "$expiry_state" = 'expired:system:identity-impersonation-expirer:'
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT action || ':' || actor_kind || ':' || actor_user_id || ':' || COALESCE(actor_session_id::text, '') FROM rust_identity_impersonation_audit WHERE impersonation_id='$impersonation_id_four' ORDER BY created_at DESC LIMIT 1" | grep -Fx 'expired:system:system:identity-impersonation-expirer:'
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT string_agg(action, ',' ORDER BY created_at) FROM rust_identity_impersonation_audit WHERE impersonation_id='$impersonation_id_four'" | grep -Fx 'started,expired'
response=$(curl --silent -o /tmp/nibleaf-impersonation-expired-session.json -w '%{http_code}' http://127.0.0.1:4311/v1/identity/session -H "cookie: nibleaf_identity=$impersonation_cookie_four")
test "$response" = 401
recovered_expired_impersonation=$(curl --fail --silent -D /tmp/nibleaf-impersonation-expired-recover.headers -X POST http://127.0.0.1:4311/v1/identity/admin/impersonations/stop -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$impersonation_cookie_four; nibleaf_identity_resume=$impersonation_resume_cookie_four")
printf '%s' "$recovered_expired_impersonation" | python3 -c 'import json,sys; assert json.load(sys.stdin)["data"]["changed"] is False'
grep -Fi 'set-cookie: nibleaf_identity=' /tmp/nibleaf-impersonation-expired-recover.headers | grep -F "nibleaf_identity=$identity_cookie" >/dev/null
worker_metrics=$(curl --fail --silent -H "$metrics_auth_header" http://127.0.0.1:4312/metrics)
printf '%s' "$worker_metrics" | grep -F 'nibleaf_identity_impersonation_expiry_reconciliations_total' >/dev/null
printf '%s' "$worker_metrics" | grep -F 'nibleaf_identity_impersonation_expired_total' >/dev/null
if psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 -c "UPDATE rust_identity_impersonation_audit SET action='ended' WHERE impersonation_id='$impersonation_id'" 2>/tmp/nibleaf-impersonation-audit-mutation.err; then
  echo 'expected immutable native impersonation audit trigger to reject UPDATE' >&2
  exit 1
fi
grep -F 'rust_identity_impersonation_audit facts are immutable' /tmp/nibleaf-impersonation-audit-mutation.err >/dev/null

# The email-change request has generic acknowledgement semantics (including a
# per-purpose rate limit) but needs native-cookie actor/CSRF admission. The
# API test process has a deliberately non-routable provider token only so this
# delivery-dependent admission can be tested without a live provider; the
# worker remains unconfigured below and is still tested for dead-lettering.
response=$(curl --silent -o /tmp/nibleaf-email-change-no-native-cookie.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/email-change/request -H 'origin: http://localhost:4310' -H 'authorization: Bearer test-owner-token' -H 'x-csrf-token: ignored' -H 'content-type: application/json' --data '{"new_email":"no-native-cookie@example.test"}')
test "$response" = 401
response=$(curl --silent -o /tmp/nibleaf-email-change-missing-csrf.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/email-change/request -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H 'content-type: application/json' --data '{"new_email":"missing-csrf@example.test"}')
test "$response" = 401
identity_user_id=$(psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -v ON_ERROR_STOP=1 -c "SELECT id FROM \"user\" WHERE email='identity-http@example.test'")
test -n "$identity_user_id"
# The browser now receives the opaque current-challenge handle generated by its
# own native request. This harness changes only the disposable test hash to a
# known code; it never reads an email-delivery ciphertext or uses a real mail
# provider. The next continuation handle comes from the verification response.
email_change_request=$(curl --fail --silent -D /tmp/nibleaf-email-change-request.headers -X POST http://127.0.0.1:4311/v1/identity/email-change/request -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf" -H 'content-type: application/json' --data '{"new_email":"identity-http-browser@example.test"}')
email_change_current_challenge=$(printf '%s' "$email_change_request" | python3 -c 'import json,sys,uuid; value=json.load(sys.stdin)["data"]["challenge_id"]; uuid.UUID(value); print(value)')
grep -Fi 'cache-control: no-store' /tmp/nibleaf-email-change-request.headers >/dev/null
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 -c "UPDATE rust_identity_otp_challenge SET code_hash=digest('654321', 'sha256') WHERE id='$email_change_current_challenge'"
email_change_verify=$(curl --fail --silent -D /tmp/nibleaf-email-change-verify-current.headers -X POST http://127.0.0.1:4311/v1/identity/email-change/verify-current -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf" -H 'content-type: application/json' --data "{\"challenge_id\":\"$email_change_current_challenge\",\"code\":\"654321\"}")
email_change_new_challenge=$(printf '%s' "$email_change_verify" | python3 -c 'import json,sys,uuid; value=json.load(sys.stdin)["data"]["challenge_id"]; uuid.UUID(value); print(value)')
grep -Fi 'cache-control: no-store' /tmp/nibleaf-email-change-verify-current.headers >/dev/null
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT state || ':' || new_challenge_id::text FROM rust_identity_email_change WHERE current_challenge_id='$email_change_current_challenge'" | grep -Fx "pending_new:$email_change_new_challenge"
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 -c "UPDATE rust_identity_otp_challenge SET code_hash=digest('112233', 'sha256') WHERE id='$email_change_new_challenge'"
response=$(curl --silent -o /tmp/nibleaf-email-change-confirm.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/email-change/confirm -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf" -H 'content-type: application/json' --data "{\"challenge_id\":\"$email_change_new_challenge\",\"code\":\"112233\"}")
test "$response" = 200
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT email FROM \"user\" WHERE id='$identity_user_id'" | grep -Fx identity-http-browser@example.test
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT state FROM rust_identity_email_change WHERE current_challenge_id='$email_change_current_challenge'" | grep -Fx completed
response=$(curl --silent -o /tmp/nibleaf-email-change-confirm-replay.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/email-change/confirm -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf" -H 'content-type: application/json' --data "{\"challenge_id\":\"$email_change_new_challenge\",\"code\":\"112233\"}")
test "$response" = 401
# Every normal request outcome returns a 202 with an opaque UUID-shaped
# continuation. The final input is rate limited, so attempting to continue it
# must produce the same generic verify response rather than an account oracle.
taken_challenge_id=''
for target in invalid-email identity-unconfigured@example.test identity-http-change-one@example.test identity-http-change-two@example.test identity-http-change-three@example.test identity-http-change-rate-limited@example.test; do
  email_change_generic=$(curl --fail --silent -X POST http://127.0.0.1:4311/v1/identity/email-change/request -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf" -H 'content-type: application/json' --data "{\"new_email\":\"$target\"}")
  generic_challenge_id=$(printf '%s' "$email_change_generic" | python3 -c 'import json,sys,uuid; value=json.load(sys.stdin)["data"]["challenge_id"]; uuid.UUID(value); print(value)')
  if [[ "$target" = identity-unconfigured@example.test ]]; then
    taken_challenge_id=$generic_challenge_id
  fi
done
test -n "$taken_challenge_id"
# A taken target still caused a current-mailbox proof to be persisted. This
# closes the delivery side channel: OTP arrival cannot reveal target ownership.
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT purpose || ':' || normalized_email FROM rust_identity_otp_challenge WHERE id='$taken_challenge_id'" | grep -Fx 'verify_email:identity-http-browser@example.test'
email_change_generic_verify=$(curl --fail --silent -X POST http://127.0.0.1:4311/v1/identity/email-change/verify-current -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf" -H 'content-type: application/json' --data "{\"challenge_id\":\"$generic_challenge_id\",\"code\":\"000000\"}")
printf '%s' "$email_change_generic_verify" | python3 -c 'import json,sys,uuid; uuid.UUID(json.load(sys.stdin)["data"]["challenge_id"])'

# The first native account-deletion surface is deliberately much narrower than
# legacy tenant erasure: it permits only a direct native user with *zero*
# memberships. These raw fixtures make every session/CSRF and outbox state
# explicit; no mail payload or production credential participates.
delete_zero_user='identity-delete-http-zero'
delete_zero_token='emVyby10b2tlbjAwMDAwMDAwMDAwMDAwMDAwMDAwMDA'
delete_zero_csrf='emVyby1jc3JmMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDA'
delete_zero_session=$(psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -v ON_ERROR_STOP=1 <<SQL
INSERT INTO "user" (id,name,email,"emailVerified",role,banned,"createdAt","updatedAt")
VALUES ('$delete_zero_user','Delete Zero','identity-delete-http-zero@example.test',TRUE,'user',FALSE,NOW(),NOW());
INSERT INTO rust_identity_session (id,user_id,token_hash,csrf_secret_hash,expires_at)
VALUES (gen_random_uuid(),'$delete_zero_user',digest(decode('$delete_zero_token=','base64'),'sha256'),digest(decode('$delete_zero_csrf=','base64'),'sha256'),NOW() + interval '1 hour')
RETURNING id;
SQL
)
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 <<SQL
INSERT INTO outbox_event (id,tenant_id,aggregate_type,aggregate_id,event_type,schema_version,payload,correlation_id,available_at)
VALUES (gen_random_uuid(),NULL,'identity_account','$delete_zero_user','identity.account.provision_requested',1,jsonb_build_object('user_id','$delete_zero_user'),gen_random_uuid(),NOW() + interval '1 hour');
SQL
# Browser mutation admission blocks the operation before the deletion feature
# transaction when either Origin or CSRF is absent; nothing may be cleared,
# cancelled, audited, or deleted on either rejection.
response=$(curl --silent -o /tmp/nibleaf-account-delete-missing-csrf.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/account-deletion -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$delete_zero_token")
test "$response" = 401
response=$(curl --silent -o /tmp/nibleaf-account-delete-missing-origin.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/account-deletion -H "cookie: nibleaf_identity=$delete_zero_token" -H "x-csrf-token: $delete_zero_csrf")
test "$response" = 403
response=$(curl --silent -o /tmp/nibleaf-account-delete-untrusted-origin.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/account-deletion -H 'origin: https://untrusted.example.test' -H "cookie: nibleaf_identity=$delete_zero_token" -H "x-csrf-token: $delete_zero_csrf")
test "$response" = 403
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM \"user\" WHERE id='$delete_zero_user'" | grep -Fx 1
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT state FROM outbox_event WHERE aggregate_type='identity_account' AND aggregate_id='$delete_zero_user'" | grep -Fx pending
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM rust_identity_account_deletion_audit WHERE deleted_user_id='$delete_zero_user'" | grep -Fx 0
response=$(curl --silent -D /tmp/nibleaf-account-delete-zero.headers -o /tmp/nibleaf-account-delete-zero.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/account-deletion -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$delete_zero_token" -H "x-csrf-token: $delete_zero_csrf")
test "$response" = 204
grep -Fi 'set-cookie: nibleaf_identity=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0' /tmp/nibleaf-account-delete-zero.headers >/dev/null
grep -Fi 'cache-control: no-store' /tmp/nibleaf-account-delete-zero.headers >/dev/null
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM \"user\" WHERE id='$delete_zero_user'" | grep -Fx 0
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT state || ':' || last_error FROM outbox_event WHERE aggregate_type='identity_account' AND aggregate_id='$delete_zero_user'" | grep -Fx 'dead_letter:identity account deleted before workspace provisioning'
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT action FROM rust_identity_account_deletion_audit WHERE deleted_user_id='$delete_zero_user'" | grep -Fx self_deleted_without_memberships
response=$(curl --silent -o /tmp/nibleaf-account-delete-removed-session.json -w '%{http_code}' http://127.0.0.1:4311/v1/identity/session -H "cookie: nibleaf_identity=$delete_zero_token")
test "$response" = 401

# A retained tenant scope is fail-closed: the native cookie stays live and the
# relational user/member rows are untouched until ownership and retained-store
# erasure parity exists.
delete_member_user='identity-delete-http-member'
delete_member_token='bWVtYmVyLXRva2VuMDAwMDAwMDAwMDAwMDAwMDAwMDA'
delete_member_csrf='bWVtYmVyLWNzcmYwMDAwMDAwMDAwMDAwMDAwMDAwMDA'
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 <<SQL
INSERT INTO "user" (id,name,email,"emailVerified",role,banned,"createdAt","updatedAt")
VALUES ('$delete_member_user','Delete Member','identity-delete-http-member@example.test',TRUE,'user',FALSE,NOW(),NOW());
INSERT INTO rust_identity_session (id,user_id,token_hash,csrf_secret_hash,expires_at)
VALUES (gen_random_uuid(),'$delete_member_user',digest(decode('$delete_member_token=','base64'),'sha256'),digest(decode('$delete_member_csrf=','base64'),'sha256'),NOW() + interval '1 hour');
INSERT INTO organization (id,name,slug) VALUES ('identity-delete-http-organization','Deletion Membership','identity-delete-http-membership');
INSERT INTO member (id,"organizationId","userId",role) VALUES ('identity-delete-http-member-row','identity-delete-http-organization','$delete_member_user','owner');
SQL
response=$(curl --silent -D /tmp/nibleaf-account-delete-member.headers -o /tmp/nibleaf-account-delete-member.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/account-deletion -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$delete_member_token" -H "x-csrf-token: $delete_member_csrf")
test "$response" = 409
if grep -Fi 'set-cookie:' /tmp/nibleaf-account-delete-member.headers >/dev/null; then
  echo 'a retained-scope deletion rejection unexpectedly cleared a cookie' >&2
  exit 1
fi
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM \"user\" WHERE id='$delete_member_user'" | grep -Fx 1
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM member WHERE \"userId\"='$delete_member_user'" | grep -Fx 1
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM rust_identity_account_deletion_audit WHERE deleted_user_id='$delete_member_user'" | grep -Fx 0

# A shared workspace survives account deletion. The last owner is transferred
# atomically before the departing membership is removed; no tenant erasure runs
# because another member retains the workspace.
delete_shared_user='identity-delete-http-shared-owner'
delete_shared_successor='identity-delete-http-shared-successor'
delete_shared_token='c2hhcmVkLXRva2VuMDAwMDAwMDAwMDAwMDAwMDAwMDA'
delete_shared_csrf='c2hhcmVkLWNzcmYwMDAwMDAwMDAwMDAwMDAwMDAwMDA'
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 <<SQL
INSERT INTO "user" (id,name,email,"emailVerified",role,banned,"createdAt","updatedAt") VALUES
 ('$delete_shared_user','Delete Shared Owner','identity-delete-http-shared-owner@example.test',TRUE,'user',FALSE,NOW(),NOW()),
 ('$delete_shared_successor','Delete Shared Successor','identity-delete-http-shared-successor@example.test',TRUE,'user',FALSE,NOW(),NOW());
INSERT INTO rust_identity_session (id,user_id,token_hash,csrf_secret_hash,expires_at)
VALUES (gen_random_uuid(),'$delete_shared_user',digest(decode('$delete_shared_token=','base64'),'sha256'),digest(decode('$delete_shared_csrf=','base64'),'sha256'),NOW() + interval '1 hour');
INSERT INTO organization (id,name,slug) VALUES ('identity-delete-http-shared-organization','Shared Deletion','identity-delete-http-shared');
INSERT INTO member (id,"organizationId","userId",role) VALUES
 ('identity-delete-http-shared-owner-member','identity-delete-http-shared-organization','$delete_shared_user','owner'),
 ('identity-delete-http-shared-successor-member','identity-delete-http-shared-organization','$delete_shared_successor','member');
SQL
response=$(curl --silent -D /tmp/nibleaf-account-delete-shared.headers -o /tmp/nibleaf-account-delete-shared.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/account-deletion -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$delete_shared_token" -H "x-csrf-token: $delete_shared_csrf")
test "$response" = 204
grep -Fi 'set-cookie: nibleaf_identity=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0' /tmp/nibleaf-account-delete-shared.headers >/dev/null
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM \"user\" WHERE id='$delete_shared_user'" | grep -Fx 0
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT role FROM member WHERE \"organizationId\"='identity-delete-http-shared-organization' AND \"userId\"='$delete_shared_successor'" | grep -Fx owner
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM member WHERE \"organizationId\"='identity-delete-http-shared-organization'" | grep -Fx 1
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT action FROM rust_identity_account_deletion_audit WHERE deleted_user_id='$delete_shared_user'" | grep -Fx self_deleted_after_membership_departure

# A leased provisioning event is owned by a worker. Deletion refuses instead of
# racing that worker, while a still-pending event above was atomically fenced.
delete_processing_user='identity-delete-http-processing'
delete_processing_token='cHJvY2Vzcy10b2tlbjAwMDAwMDAwMDAwMDAwMDAwMDA'
delete_processing_csrf='cHJvY2Vzcy1jc3JmMDAwMDAwMDAwMDAwMDAwMDAwMDA'
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 <<SQL
INSERT INTO "user" (id,name,email,"emailVerified",role,banned,"createdAt","updatedAt")
VALUES ('$delete_processing_user','Delete Processing','identity-delete-http-processing@example.test',TRUE,'user',FALSE,NOW(),NOW());
INSERT INTO rust_identity_session (id,user_id,token_hash,csrf_secret_hash,expires_at)
VALUES (gen_random_uuid(),'$delete_processing_user',digest(decode('$delete_processing_token=','base64'),'sha256'),digest(decode('$delete_processing_csrf=','base64'),'sha256'),NOW() + interval '1 hour');
INSERT INTO outbox_event (id,tenant_id,aggregate_type,aggregate_id,event_type,schema_version,payload,correlation_id,state,lease_token,leased_until,worker_id)
VALUES (gen_random_uuid(),NULL,'identity_account','$delete_processing_user','identity.account.provision_requested',1,jsonb_build_object('user_id','$delete_processing_user'),gen_random_uuid(),'processing',gen_random_uuid(),NOW() + interval '1 hour','deletion-test-worker');
SQL
response=$(curl --silent -o /tmp/nibleaf-account-delete-processing.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/account-deletion -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$delete_processing_token" -H "x-csrf-token: $delete_processing_csrf")
test "$response" = 409
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM \"user\" WHERE id='$delete_processing_user'" | grep -Fx 1
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT state FROM outbox_event WHERE aggregate_type='identity_account' AND aggregate_id='$delete_processing_user'" | grep -Fx processing

# Revoked native sessions cannot reach deletion and cannot create evidence.
delete_stale_user='identity-delete-http-stale'
delete_stale_token='c3RhbGUtdG9rZW4wMDAwMDAwMDAwMDAwMDAwMDAwMDA'
delete_stale_csrf='c3RhbGUtY3NyZjAwMDAwMDAwMDAwMDAwMDAwMDAwMDA'
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 <<SQL
INSERT INTO "user" (id,name,email,"emailVerified",role,banned,"createdAt","updatedAt")
VALUES ('$delete_stale_user','Delete Stale','identity-delete-http-stale@example.test',TRUE,'user',FALSE,NOW(),NOW());
INSERT INTO rust_identity_session (id,user_id,token_hash,csrf_secret_hash,expires_at,revoked_at,revoke_reason)
VALUES (gen_random_uuid(),'$delete_stale_user',digest(decode('$delete_stale_token=','base64'),'sha256'),digest(decode('$delete_stale_csrf=','base64'),'sha256'),NOW() + interval '1 hour',NOW(),'deletion_test_stale');
SQL
response=$(curl --silent -o /tmp/nibleaf-account-delete-stale.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/account-deletion -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$delete_stale_token" -H "x-csrf-token: $delete_stale_csrf")
test "$response" = 401
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM \"user\" WHERE id='$delete_stale_user'" | grep -Fx 1
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM rust_identity_account_deletion_audit WHERE deleted_user_id='$delete_stale_user'" | grep -Fx 0

response=$(curl --silent -o /tmp/nibleaf-identity-signout-missing-csrf.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/sign-out -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie")
test "$response" = 401
response=$(curl --silent -D /tmp/nibleaf-identity-signout.headers -o /tmp/nibleaf-identity-signout.json -w '%{http_code}' -X POST http://127.0.0.1:4311/v1/identity/sign-out -H 'origin: http://localhost:4310' -H "cookie: nibleaf_identity=$identity_cookie" -H "x-csrf-token: $identity_csrf")
test "$response" = 204
grep -Fi 'set-cookie: nibleaf_identity=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0' /tmp/nibleaf-identity-signout.headers >/dev/null
response=$(curl --silent -o /tmp/nibleaf-identity-session-revoked.json -w '%{http_code}' http://127.0.0.1:4311/v1/identity/session -H "cookie: nibleaf_identity=$identity_cookie")
test "$response" = 401

# ADR-0007: request idempotency, canonical recipient binding, 404 anti-oracle
# acceptance behavior, cancellation, and replay-safe acceptance are tested over
# the running API against the actual PostgreSQL migrations.
manual_invitation=$(curl --fail --silent -X POST "http://127.0.0.1:4311/v1/projects/$project_id/member-invitations" \
  -H 'authorization: Bearer test-owner-token' -H 'Idempotency-Key: invitation-manual-outsider' -H 'content-type: application/json' \
  --data '{"recipient_email":" Outsider@Example.Test ","role":"admin","delivery_mode":"manual"}')
manual_invitation_id=$(printf '%s' "$manual_invitation" | python3 -c 'import json,sys; body=json.load(sys.stdin)["data"]; assert body["status"] == "pending" and body["recipient_email"] == "outsider@example.test"; print(body["id"])')
manual_acceptance_url=$(printf '%s' "$manual_invitation" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["acceptance_url"])')
manual_retry=$(curl --fail --silent -X POST "http://127.0.0.1:4311/v1/projects/$project_id/member-invitations" \
  -H 'authorization: Bearer test-owner-token' -H 'Idempotency-Key: invitation-manual-outsider' -H 'content-type: application/json' \
  --data '{"recipient_email":"outsider@example.test","role":"admin","delivery_mode":"manual"}')
printf '%s' "$manual_retry" | python3 -c "import json,sys; assert json.load(sys.stdin)['data']['id'] == '$manual_invitation_id'"
response=$(curl --silent -o /tmp/nibleaf-invitation-email-mismatch.json -w '%{http_code}' \
  -X POST "http://127.0.0.1:4311/v1/member-invitations/${manual_acceptance_url##*/}/accept" -H 'authorization: Bearer test-owner-token')
test "$response" = 404
accepted_invitation=$(curl --fail --silent -X POST "http://127.0.0.1:4311/v1/member-invitations/${manual_acceptance_url##*/}/accept" -H 'authorization: Bearer test-outsider-token')
printf '%s' "$accepted_invitation" | python3 -c "import json,sys; body=json.load(sys.stdin)['data']; assert body['id'] == '$manual_invitation_id' and body['status'] == 'accepted'"
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT status FROM member_invitation WHERE id='$manual_invitation_id'" | grep -Fx accepted
curl --fail --silent -X POST "http://127.0.0.1:4311/v1/member-invitations/${manual_acceptance_url##*/}/accept" -H 'authorization: Bearer test-outsider-token' >/dev/null
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM member WHERE \"organizationId\"=(SELECT \"organizationId\" FROM project WHERE id='$project_id') AND \"userId\"='test-outsider'" | grep -Fx 1
cancelled_invitation=$(curl --fail --silent -X POST "http://127.0.0.1:4311/v1/projects/$project_id/member-invitations" \
  -H 'authorization: Bearer test-owner-token' -H 'Idempotency-Key: invitation-manual-cancel' -H 'content-type: application/json' \
  --data '{"recipient_email":"cancelled@example.test","role":"member","delivery_mode":"manual"}')
cancelled_invitation_id=$(printf '%s' "$cancelled_invitation" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["id"])')
curl --fail --silent -X POST "http://127.0.0.1:4311/v1/projects/$project_id/member-invitations/$cancelled_invitation_id/cancel" -H 'authorization: Bearer test-owner-token' >/dev/null
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT status FROM member_invitation WHERE id='$cancelled_invitation_id'" | grep -Fx cancelled
# An email-mode invitation must persist delivery state and the worker must turn
# missing-provider configuration into explicit dead-letter evidence.
email_invitation=$(curl --fail --silent -X POST "http://127.0.0.1:4311/v1/projects/$project_id/member-invitations" \
  -H 'authorization: Bearer test-owner-token' -H 'Idempotency-Key: invitation-email-unconfigured' -H 'content-type: application/json' \
  --data '{"recipient_email":"provider-failure@example.test","role":"member","delivery_mode":"email"}')
email_invitation_id=$(printf '%s' "$email_invitation" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["id"])')
sleep 1
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT state FROM member_invitation_delivery WHERE invitation_id='$email_invitation_id'" | grep -Fx dead_letter
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT state FROM outbox_event WHERE event_type='membership.invitation.delivery.requested' AND aggregate_id='$email_invitation_id'" | grep -Fx dead_letter

language_id=$(psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT id FROM language WHERE \"projectId\"='$project_id' AND \"isDefault\"=TRUE")
page=$(curl --fail --silent -X POST "http://127.0.0.1:4311/v1/projects/$project_id/pages" \
  -H 'authorization: Bearer test-owner-token' -H 'content-type: application/json' \
  --data "{\"language_id\":\"$language_id\",\"title\":\"Welcome\",\"slug\":\"welcome\",\"content\":\"# Welcome\"}")
page_id=$(printf '%s' "$page" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["id"])')
response=$(curl --silent -o /tmp/nibleaf-stale.json -w '%{http_code}' -X PATCH "http://127.0.0.1:4311/v1/projects/$project_id/pages/$page_id" \
  -H 'authorization: Bearer test-owner-token' -H 'content-type: application/json' \
  --data '{"expected_revision":9,"content":"stale"}')
test "$response" = 409
response=$(curl --silent -o /tmp/nibleaf-tenant.json -w '%{http_code}' "http://127.0.0.1:4311/v1/projects/$project_id/onboarding" -H 'authorization: Bearer test-unrelated-token')
test "$response" = 404

# The invitation acceptance above created an admin in the project tenant; reuse
# that member for the role and ownership-transition checks below.
target_member_id=$(psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT id FROM member WHERE \"organizationId\"=(SELECT \"organizationId\" FROM project WHERE id='$project_id') AND \"userId\"='test-outsider'")
test -n "$target_member_id"
members=$(curl --fail --silent "http://127.0.0.1:4311/v1/projects/$project_id/members" -H 'authorization: Bearer test-owner-token')
printf '%s' "$members" | python3 -c "import json,sys; assert any(row['id'] == '$target_member_id' and row['role'] == 'admin' for row in json.load(sys.stdin)['data'])"
curl --fail --silent -X PATCH "http://127.0.0.1:4311/v1/projects/$project_id/members/$target_member_id" \
  -H 'authorization: Bearer test-owner-token' -H 'content-type: application/json' --data '{"role":"member"}' >/dev/null
response=$(curl --silent -o /tmp/nibleaf-member-role.json -w '%{http_code}' -X PATCH "http://127.0.0.1:4311/v1/projects/$project_id/members/$target_member_id" \
  -H 'authorization: Bearer test-outsider-token' -H 'content-type: application/json' --data '{"role":"admin"}')
test "$response" = 404
curl --fail --silent -X PATCH "http://127.0.0.1:4311/v1/projects/$project_id/members/$target_member_id" \
  -H 'authorization: Bearer test-owner-token' -H 'content-type: application/json' --data '{"role":"admin"}' >/dev/null
response=$(curl --silent -D /tmp/nibleaf-owner-grant.headers -o /tmp/nibleaf-owner-grant.json -w '%{http_code}' -X PATCH "http://127.0.0.1:4311/v1/projects/$project_id/members/$target_member_id" \
  -H 'authorization: Bearer test-owner-token' -H 'content-type: application/json' --data '{"role":"owner"}')
test "$response" = 422
grep -Fi 'content-type: application/problem+json' /tmp/nibleaf-owner-grant.headers >/dev/null
python3 - <<'PY'
import json
with open('/tmp/nibleaf-owner-grant.json', encoding='utf-8') as response:
    problem = json.load(response)
assert problem['status'] == 422
assert problem['type'].endswith('/validation')
PY
# Two concurrent transfers from the same owner must serialize. One observes the
# owner role and succeeds; the other re-authorizes after the advisory lock and
# receives the same tenant-safe 404 as any non-owner.
for attempt in 1 2; do
  curl --silent -o "/tmp/nibleaf-transfer-$attempt.json" -w '%{http_code}' \
    -X POST "http://127.0.0.1:4311/v1/projects/$project_id/members/transfer-ownership" \
    -H 'authorization: Bearer test-owner-token' -H 'content-type: application/json' \
    --data "{\"member_id\":\"$target_member_id\"}" > "/tmp/nibleaf-transfer-$attempt.status" &
  transfer_pids[$attempt]=$!
done
for attempt in 1 2; do
  wait "${transfer_pids[$attempt]}"
done
test "$(sort /tmp/nibleaf-transfer-1.status /tmp/nibleaf-transfer-2.status | tr '\n' ' ')" = '200 404 '
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM member WHERE \"organizationId\" = (SELECT \"organizationId\" FROM project WHERE id = '$project_id') AND role = 'owner'" | grep -Fx 1
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT role FROM member WHERE id = '$target_member_id'" | grep -Fx owner
response=$(curl --silent -o /tmp/nibleaf-former-owner.json -w '%{http_code}' -X POST "http://127.0.0.1:4311/v1/projects/$project_id/members/transfer-ownership" \
  -H 'authorization: Bearer test-owner-token' -H 'content-type: application/json' --data "{\"member_id\":\"$target_member_id\"}")
test "$response" = 404

api_metrics=$(curl --fail --silent -H "$metrics_auth_header" http://127.0.0.1:4311/metrics)
printf '%s' "$api_metrics" | grep -F 'route="/v1/projects/{project_id}/pages"' >/dev/null
printf '%s' "$api_metrics" | grep -E 'nibleaf_database_pool_connections\{[^}]*state="total"' >/dev/null
if printf '%s' "$api_metrics" | grep -F "$project_id" >/dev/null; then
  echo 'raw project ID appeared in Prometheus metric labels' >&2
  exit 1
fi
# This HTTP/worker proof captures every branch/locale and visibility fact,
# rejects cross-project language references before acceptance, mutates later
# drafts, and checks exact format-2 results from the immutable generation.
bash "$rust_root/scripts/test-search-publication-scope.sh" "$project_id" "$language_id" "$starter_project_id"
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT state FROM rust_search_generation WHERE project_id='$project_id'" | grep -Fx ready
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT count(*) FROM rust_search_active_generation WHERE project_id='$project_id'" | grep -Fx 1
# The authenticated management search route opens only the registry-selected
# project generation on the configured shared root. A non-member gets the same
# tenant-safe 404 as other project feature routes and cannot use a project ID as
# an index-scope oracle.
project_search=$(curl --fail --silent -D /tmp/nibleaf-project-search.headers "http://127.0.0.1:4311/v1/projects/$project_id/search?q=Welcome" -H 'authorization: Bearer test-owner-token')
grep -Fi 'cache-control: private, no-store' /tmp/nibleaf-project-search.headers >/dev/null
printf '%s' "$project_search" | python3 -c 'import json,sys; data=json.load(sys.stdin)["data"]; assert data["hits"] and data["hits"][0]["title"] == "Welcome"; assert data["deployment_id"] and data["generation_id"]'
response=$(curl --silent -o /tmp/nibleaf-project-search-cross-tenant.json -w '%{http_code}' "http://127.0.0.1:4311/v1/projects/$project_id/search?q=Welcome" -H 'authorization: Bearer test-unrelated-token')
test "$response" = 404
psql "$NIBLEAF_TEST_DATABASE_URL" -At -c "SELECT state FROM onboarding_run WHERE project_id='$project_id'" | grep -Fx complete
# Query admission/size bounds, startup validation and DEBUG log/metric privacy.
bash "$rust_root/scripts/test-search-query-runtime.sh" "$project_id" "$search_index_root"
# Explicitly enabled candidate only; the main API remains default-disabled.
python3 "$rust_root/scripts/test-public-search-http.py" "$project_id" "$search_index_root"
# Stop/reap the normal worker only after all earlier workflows/metrics proofs;
# a separate finite worker proves accepted delivery survives SIGTERM.
python3 "$rust_root/scripts/test-worker-drain.py" "$worker_pid" "$search_index_root"
wait "$worker_pid"
worker_pid=''
python3 "$rust_root/scripts/test-object-distribution-http.py"
# Actor lookup joins the current user row on every request, so an account
# suspension revokes this transition adapter without waiting for token expiry.
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 -c "UPDATE \"user\" SET \"suspendedAt\" = NOW() WHERE id = 'test-owner';"
response=$(curl --silent -o /tmp/nibleaf-suspended-user.json -w '%{http_code}' \
  http://127.0.0.1:4311/v1/projects -H 'authorization: Bearer test-owner-token')
test "$response" = 401

echo 'PostgreSQL integration: passed'
