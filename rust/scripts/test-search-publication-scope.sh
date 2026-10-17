#!/usr/bin/env bash
# Called by test-postgres-integration.sh after its API/worker and isolated
# project/member fixtures exist. No production URL is accepted.
set -euo pipefail
: "${NIBLEAF_TEST_DATABASE_URL:?Disposable integration database URL required}"
[[ "$NIBLEAF_TEST_DATABASE_URL" == *nibleaf_rust_test* ]] || exit 2
project_id=${1:?project fixture ID required}
language_id=${2:?default language fixture ID required}
foreign_project_id=${3:?unrelated project fixture ID required}
base_url='http://127.0.0.1:4311'
auth_header='authorization: Bearer test-owner-token'
branch_id=$(psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -v project_id="$project_id" <<'SQL'
SELECT id FROM branch WHERE "projectId"=:'project_id' AND "isDefault"=TRUE;
SQL
)

# Legacy FKs permit a same-project page to reference another project's locale.
# Capture must reject the entire request before deployment/outbox acceptance,
# not silently omit the corrupt row with a scoped JOIN.
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 -v project_id="$project_id" -v foreign_project_id="$foreign_project_id" -v branch_id="$branch_id" <<'SQL'
INSERT INTO page (id,"projectId","languageId","branchId",kind,title,slug,path,content,hidden,"createdAt","updatedAt")
SELECT 'search-invalid-' || :'project_id', :'project_id', id, :'branch_id', 'PAGE'::"PageKind",
       'Invalid foreign locale','invalid-foreign-locale','invalid-foreign-locale','not searchable',FALSE,NOW(),NOW()
FROM language WHERE "projectId"=:'foreign_project_id' AND "isDefault"=TRUE;
SQL
status=$(curl --silent -o /tmp/nibleaf-search-invalid-capture.json -w '%{http_code}' -X POST "$base_url/v1/projects/$project_id/publications" -H "$auth_header" -H 'content-type: application/json' --data '{}')
test "$status" = 409
# The independent branch FK is subject to the same tenant boundary.
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 -v project_id="$project_id" -v language_id="$language_id" -v foreign_project_id="$foreign_project_id" <<'SQL'
UPDATE page SET "languageId"=:'language_id',
    "branchId"=(SELECT id FROM branch WHERE "projectId"=:'foreign_project_id' AND "isDefault"=TRUE)
WHERE id='search-invalid-' || :'project_id';
SQL
status=$(curl --silent -o /tmp/nibleaf-search-invalid-branch-capture.json -w '%{http_code}' -X POST "$base_url/v1/projects/$project_id/publications" -H "$auth_header" -H 'content-type: application/json' --data '{}')
test "$status" = 409
psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -v ON_ERROR_STOP=1 -v project_id="$project_id" <<'SQL' | grep -Fx '0:0'
SELECT (SELECT count(*) FROM deployment WHERE "projectId"=:'project_id') || ':' ||
       (SELECT count(*) FROM outbox_event WHERE event_type='search.generation.requested' AND payload->>'project_id'=:'project_id');
SQL
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 -v project_id="$project_id" -v language_id="$language_id" -v branch_id="$branch_id" <<'SQL'
DELETE FROM page WHERE id='search-invalid-' || :'project_id';
UPDATE project SET config=jsonb_set(COALESCE(config,'{}'::jsonb),'{variables}',
    '[{"key":"product","value":"Frozen Nibleaf"}]'::jsonb) WHERE id=:'project_id';
INSERT INTO branch (id,"projectId",name,"isDefault","createdAt","updatedAt")
VALUES ('search-v2-' || :'project_id',:'project_id','Release v2',FALSE,NOW(),NOW());
INSERT INTO language (id,"projectId",code,label,direction,"isDefault",enabled,position,"createdAt","updatedAt") VALUES
 ('search-fr-' || :'project_id',:'project_id','fr','French','LTR',FALSE,TRUE,1,NOW(),NOW()),
 ('search-gb-' || :'project_id',:'project_id','en-GB','British English','LTR',FALSE,TRUE,2,NOW(),NOW()),
 ('search-disabled-' || :'project_id',:'project_id','ja','Japanese','LTR',FALSE,FALSE,3,NOW(),NOW());
INSERT INTO page (id,"projectId","languageId","branchId",kind,title,slug,path,content,description,icon,config,hidden,position,"createdAt","updatedAt",revision)
SELECT 'search-' || fixture.tag || '-' || :'project_id', :'project_id', fixture.language_id, fixture.branch_id,
       fixture.kind::"PageKind", '{{ product }} guide', 'scope-' || fixture.tag, 'scope-' || fixture.tag,
       'Read **scopeprobe** [manual](https://example.test/private) with {{ product }}.',
       'Frozen {{product}} summaryprobe', 'book',
       jsonb_build_object('sidebarTitle','aliasprobe','seo',jsonb_build_object('noindex',fixture.noindex)),
       fixture.hidden,0,NOW(),NOW(),0
FROM (VALUES
 ('en',:'language_id',:'branch_id','PAGE',FALSE,FALSE),
 ('fr','search-fr-' || :'project_id',:'branch_id','PAGE',FALSE,FALSE),
 ('gb','search-gb-' || :'project_id',:'branch_id','PAGE',FALSE,FALSE),
 ('v2',:'language_id','search-v2-' || :'project_id','PAGE',FALSE,FALSE),
 ('hidden',:'language_id',:'branch_id','PAGE',TRUE,FALSE),
 ('noindex',:'language_id',:'branch_id','PAGE',FALSE,TRUE),
 ('disabled','search-disabled-' || :'project_id',:'branch_id','PAGE',FALSE,FALSE),
 ('group',:'language_id',:'branch_id','GROUP',FALSE,FALSE)
) AS fixture(tag,language_id,branch_id,kind,hidden,noindex);
SQL
status=$(curl --silent -o /tmp/nibleaf-search-scope-publication.json -w '%{http_code}' -X POST "$base_url/v1/projects/$project_id/publications" -H "$auth_header" -H 'content-type: application/json' --data '{"commit_message":"integration publication"}')
test "$status" = 202
psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -v ON_ERROR_STOP=1 -v project_id="$project_id" <<'SQL' > /tmp/nibleaf-search-scope-snapshot.json
SELECT snapshot::text FROM deployment WHERE "projectId"=:'project_id';
SQL
python3 - "$project_id" <<'PY'
import json, sys
snapshot = json.load(open('/tmp/nibleaf-search-scope-snapshot.json', encoding='utf-8'))
project_id = sys.argv[1]
assert snapshot['schema_version'] == 2
assert snapshot['project']['id'] == project_id
assert len(snapshot['versions']) == 2 and sum(v['is_default'] for v in snapshot['versions']) == 1
assert any(v['slug'] == 'release-v2' for v in snapshot['versions'])
assert len(snapshot['languages']) == 4
assert next(l for l in snapshot['languages'] if l['code'] == 'ja')['enabled'] is False
assert len(snapshot['pages']) == 9
pages = {p['id']: p for p in snapshot['pages']}
page = pages[f'search-en-{project_id}']
assert page['title'] == 'Frozen Nibleaf guide'
assert page['description'] == 'Frozen Frozen Nibleaf summaryprobe'
assert '{{' not in page['content'] and 'Frozen Nibleaf' in page['content']
assert page['config']['sidebarTitle'] == 'aliasprobe'
assert page['icon'] == 'book'
assert pages[f'search-hidden-{project_id}']['hidden'] is True
assert pages[f'search-noindex-{project_id}']['config']['seo']['noindex'] is True
assert all('language_project_id' not in p and 'branch_project_id' not in p for p in pages.values())
PY

# A later draft edit must not change the captured corpus, including version
# names, disabled-language state, description/icon, and visibility facts.
psql "$NIBLEAF_TEST_DATABASE_URL" -q -v ON_ERROR_STOP=1 -v project_id="$project_id" <<'SQL'
UPDATE page SET title='Draft changed',content='draftonlyprobe',description='Draft summary',icon='draft',hidden=TRUE,
    config='{"seo":{"noindex":true}}'::jsonb WHERE "projectId"=:'project_id' AND id LIKE 'search-%';
UPDATE language SET enabled=FALSE WHERE "projectId"=:'project_id' AND code='fr';
UPDATE branch SET name='Draft renamed version' WHERE id='search-v2-' || :'project_id';
SQL
for _ in $(seq 1 30); do
  publication_status=$(psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -v project_id="$project_id" <<'SQL'
SELECT status::text FROM deployment WHERE "projectId"=:'project_id';
SQL
)
  [[ "$publication_status" == 'READY' ]] && break
  [[ "$publication_status" != 'FAILED' ]] || { echo 'Search scope publication failed' >&2; exit 1; }
  sleep 1
done
test "$publication_status" = READY
psql "$NIBLEAF_TEST_DATABASE_URL" -qAt -v ON_ERROR_STOP=1 -v project_id="$project_id" <<'SQL' | grep -Fx 5
SELECT document_count FROM rust_search_generation WHERE project_id=:'project_id' AND state='ready';
SQL

search_query() {
  curl --fail --silent --get -D /tmp/nibleaf-search-scope.headers "$base_url/v1/projects/$project_id/search" \
    -H "$auth_header" --data-urlencode "q=$1" --data-urlencode "version=$2" --data-urlencode "language=$3" --data-urlencode 'limit=50'
}
for scope in 'main en en' 'main fr fr' 'main en-GB gb' 'release-v2 en v2'; do
  read -r version language tag <<< "$scope"
  hits=$(search_query scopeprobe "$version" "$language")
  grep -Fi 'cache-control: private, no-store' /tmp/nibleaf-search-scope.headers >/dev/null
  printf '%s' "$hits" | python3 -c 'import json,sys; h=json.load(sys.stdin)["data"]["hits"]; assert len(h)==1; hit=h[0]; assert hit["page_id"]=="search-"+sys.argv[1]+"-"+sys.argv[2]; assert hit["language"]==sys.argv[3] and hit["version_slug"]==sys.argv[4]; assert hit["title"]=="Frozen Nibleaf guide"; assert hit["description"]=="Frozen Frozen Nibleaf summaryprobe" and hit["icon"]=="book"; assert "scopeprobe" in hit["snippet"] and "https://" not in hit["snippet"]; assert len(hit["snippet"])<=242' "$tag" "$project_id" "$language" "$version"
done
for query in summaryprobe aliasprobe; do
  hits=$(search_query "$query" main en)
  printf '%s' "$hits" | python3 -c 'import json,sys; h=json.load(sys.stdin)["data"]["hits"]; assert len(h)==1'
done
for scope in 'main ja' 'main unknown' 'unknown en'; do
  read -r version language <<< "$scope"
  hits=$(search_query scopeprobe "$version" "$language")
  printf '%s' "$hits" | python3 -c 'import json,sys; assert json.load(sys.stdin)["data"]["hits"] == []'
done
hits=$(search_query draftonlyprobe main en)
printf '%s' "$hits" | python3 -c 'import json,sys; assert json.load(sys.stdin)["data"]["hits"] == []'
status=$(curl --silent -o /tmp/nibleaf-search-scope-client-grants.json -w '%{http_code}' "$base_url/v1/projects/$project_id/search?q=scopeprobe&allowed_page_ids=foreign" -H "$auth_header")
test "$status" = 422
# There is intentionally no native public search HTTP surface in this milestone.
status=$(curl --silent -o /tmp/nibleaf-search-still-not-public.json -w '%{http_code}' "$base_url/v1/public/sites/$project_id/search?q=scopeprobe")
test "$status" = 404
echo 'Native search publication scope: passed'
