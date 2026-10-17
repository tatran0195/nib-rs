#!/usr/bin/env bash
# Finite, disposable-DB HTTP/config/privacy proof; invoked by the full harness.
set -euo pipefail
: "${NIBLEAF_TEST_DATABASE_URL:?Disposable integration database URL required}"
[[ "$NIBLEAF_TEST_DATABASE_URL" == *nibleaf_rust_test* ]] || exit 2
project_id=${1:?project fixture ID required}
index_root=${2:?test generation root required}
rust_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
extra_pid=''
cleanup() {
  [[ -z "$extra_pid" ]] || kill "$extra_pid" 2>/dev/null || true
  [[ -z "$extra_pid" ]] || wait "$extra_pid" 2>/dev/null || true
}
trap cleanup EXIT

# Every credential below belongs only to this disposable test. Configure all
# API-only query knobs explicitly so an operator shell cannot weaken the proof.
api_env=(env
  "NIBLEAF_DATABASE_URL=$NIBLEAF_TEST_DATABASE_URL"
  'NIBLEAF_METRICS_BEARER_TOKEN=query-runtime-test-metrics'
  'NIBLEAF_INVITATION_SIGNING_KEY=MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY='
  'NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY=ZmVkY2JhOTg3NjU0MzIxMGZlZGNiYTk4NzY1NDMyMTA='
  'NIBLEAF_NATIVE_IDENTITY_ENABLED=false'
  'NIBLEAF_BIND_ADDR=127.0.0.1:4315'
  "NIBLEAF_SEARCH_INDEX_ROOT=$index_root"
  'NIBLEAF_SEARCH_QUERY_CONCURRENCY=2'
  'NIBLEAF_SEARCH_QUERY_TIMEOUT_MS=5000'
  'NIBLEAF_SEARCH_QUERY_MAX_SNAPSHOT_BYTES=16777216'
  'NIBLEAF_SEARCH_QUERY_MAX_MANIFEST_BYTES=16777216'
  'NIBLEAF_SEARCH_QUERY_MAX_DOCUMENTS=50000'
  'NIBLEAF_SEARCH_QUERY_MAX_SOURCE_BYTES=67108864'
)
for setting in NIBLEAF_SEARCH_QUERY_CONCURRENCY NIBLEAF_SEARCH_QUERY_TIMEOUT_MS \
    NIBLEAF_SEARCH_QUERY_MAX_SNAPSHOT_BYTES NIBLEAF_SEARCH_QUERY_MAX_MANIFEST_BYTES \
    NIBLEAF_SEARCH_QUERY_MAX_DOCUMENTS NIBLEAF_SEARCH_QUERY_MAX_SOURCE_BYTES; do
  status=0
  timeout 10s "${api_env[@]}" "$setting=0" "$rust_root/target/debug/nibleaf-api" \
    > /tmp/nibleaf-search-query-invalid.log 2>&1 || status=$?
  test "$status" = 1
  grep -F "$setting" /tmp/nibleaf-search-query-invalid.log >/dev/null
done

# A valid but deliberately tiny operational limit starts normally and refuses
# this larger generation with 503 Problem Details, not partial hits or 422.
"${api_env[@]}" NIBLEAF_SEARCH_QUERY_MAX_DOCUMENTS=1 "$rust_root/target/debug/nibleaf-api" \
  > /tmp/nibleaf-search-query-limited-api.log 2>&1 &
extra_pid=$!
ready=false
for _ in {1..100}; do
  if curl --fail --silent http://127.0.0.1:4315/health/ready >/dev/null; then ready=true; break; fi
  kill -0 "$extra_pid"
  sleep 0.05
done
test "$ready" = true
status=$(curl --silent -o /tmp/nibleaf-search-query-limit.json -w '%{http_code}' \
  "http://127.0.0.1:4315/v1/projects/$project_id/search?q=Welcome" \
  -H 'authorization: Bearer test-owner-token')
test "$status" = 503
python3 - <<'PY'
import json
with open('/tmp/nibleaf-search-query-limit.json', encoding='utf-8') as handle:
    problem = json.load(handle)
assert problem['status'] == 503
assert problem['type'].endswith('/dependency-unavailable')
assert 'data' not in problem
PY
kill "$extra_pid"
wait "$extra_pid"
extra_pid=''
grep -F 'API HTTP and search work drained' /tmp/nibleaf-search-query-limited-api.log >/dev/null

# The main API is launched with application/Tower DEBUG logging. Exercise a
# real management query, arbitrary fallback path and custom method. None of
# the sentinel query/cookie/token/path/method text may reach traces or metrics.
status=$(curl --silent -o /tmp/nibleaf-search-private-query.json -w '%{http_code}' \
  "http://127.0.0.1:4311/v1/projects/$project_id/search?q=privacyquerysentinel" \
  -H 'authorization: Bearer test-owner-token' -H 'cookie: unrelated=privacy-cookie-sentinel')
test "$status" = 200
status=$(curl --silent -o /tmp/nibleaf-search-private-fallback.json -w '%{http_code}' \
  -X PRIVACYMETHODSENTINEL \
  'http://127.0.0.1:4311/privacy-path-sentinel?q=privacyquerysentinel' \
  -H 'authorization: Bearer privacy-token-sentinel' -H 'cookie: unrelated=privacy-cookie-sentinel')
test "$status" = 404
grep -F 'http_request' /tmp/nibleaf-rust-api.log >/dev/null
if grep -E 'privacyquerysentinel|privacy-(cookie|token|path)-sentinel|PRIVACYMETHODSENTINEL' /tmp/nibleaf-rust-api.log; then
  echo 'private HTTP sentinel appeared in DEBUG-level API logs' >&2
  exit 1
fi
metrics=$(curl --fail --silent -H 'authorization: Bearer test-metrics-token' http://127.0.0.1:4311/metrics)
printf '%s\n' "$metrics" > /tmp/nibleaf-search-query-metrics.prom
printf '%s' "$metrics" | grep -F 'nibleaf_search_query_finished_total' >/dev/null
printf '%s' "$metrics" | grep -F 'nibleaf_search_query_lifecycle_seconds' >/dev/null
printf '%s' "$metrics" | grep -F 'method="OTHER"' >/dev/null
if printf '%s' "$metrics" | grep -E 'privacyquerysentinel|privacy-(cookie|token|path)-sentinel|PRIVACYMETHODSENTINEL'; then
  echo 'private sentinel appeared in metric labels' >&2
  exit 1
fi
echo 'Search query runtime HTTP: invalid startup limits, 503 bounds, graceful drain, DEBUG trace/metric privacy passed'
