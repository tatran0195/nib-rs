#!/usr/bin/env python3
"""Finite disposable-DB opt-in HTTP contract, authorization and fault proof."""
import base64, concurrent.futures, hashlib, http.client, json, os, pathlib, re, secrets, socket, subprocess, sys, time, urllib.parse, urllib.request, urllib.error, uuid

DB = os.environ['NIBLEAF_TEST_DATABASE_URL']
assert urllib.parse.urlsplit(DB).path == '/nibleaf_rust_test'
assert urllib.parse.urlsplit(DB).hostname in ('127.0.0.1', 'localhost')
PROJECT, ROOT = sys.argv[1:3]
assert re.fullmatch(r'[a-zA-Z0-9_-]+', PROJECT)
RUST = pathlib.Path(__file__).resolve().parents[1]
ENV = {**os.environ, 'NIBLEAF_DATABASE_URL': DB, 'NIBLEAF_BIND_ADDR': '127.0.0.1:4316',
       'NIBLEAF_METRICS_BEARER_TOKEN': 'public-proof-metrics',
       'NIBLEAF_INVITATION_SIGNING_KEY': 'MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=',
       'NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY': 'ZmVkY2JhOTg3NjU0MzIxMGZlZGNiYTk4NzY1NDMyMTA=',
       'NIBLEAF_NATIVE_IDENTITY_ENABLED': 'true', 'NIBLEAF_PUBLIC_SEARCH_ENABLED': 'true', 'NIBLEAF_AUTHORITY': 'transition',
       'NIBLEAF_SEARCH_INDEX_ROOT': ROOT, 'NIBLEAF_SEARCH_QUERY_TIMEOUT_MS': '1500',
       'RUST_LOG': 'info,nibleaf_api=debug,tower_http=debug'}
LOG = pathlib.Path('/tmp/nibleaf-public-search-api.log')
BASE = 'http://127.0.0.1:4316'
PATH = f'/v1/public/sites/{PROJECT}/search'
CHECKS = 0

def sql(statement):
    return subprocess.check_output(['psql', DB, '-XqAt', '-v', 'ON_ERROR_STOP=1', '-c', statement], text=True).strip()
def quoted(value):
    return "'" + value.replace("'", "''") + "'"
def request(path, code=200, headers=None, method='GET', public=True):
    global CHECKS
    req = urllib.request.Request(BASE + path, headers=headers or {}, method=method)
    try:
        response = urllib.request.urlopen(req, timeout=10)
    except urllib.error.HTTPError as error:
        response = error
    body = response.read()
    assert response.status == code, (path.split('?')[0], response.status, code, body[:180])
    if public:
        assert 'no-store' in response.headers['Cache-Control']
        vary = response.headers['Vary'].lower()
        assert 'cookie' in vary and 'authorization' in vary
    value = json.loads(body) if body else None
    if code >= 400:
        assert value['status'] == code and 'data' not in value
    CHECKS += 1
    return value

def mode(value):
    sql(f'UPDATE project SET "accessMode"={quoted(value)} WHERE id={quoted(PROJECT)}')

def wait_ready(process):
    for _ in range(150):
        assert process.poll() is None
        try:
            urllib.request.urlopen(BASE + '/health/ready', timeout=.3).close()
            return
        except (OSError, urllib.error.URLError):
            time.sleep(.05)
    raise AssertionError('candidate API did not become ready')

def main():
    # Invalid/empty rollout values must not silently expose the route.
    for flag in ['', 'TRUE', '1']:
        failed = subprocess.run([str(RUST/'target/debug/nibleaf-api')], env={**ENV,'NIBLEAF_PUBLIC_SEARCH_ENABLED':flag}, capture_output=True, timeout=10)
        assert failed.returncode == 1 and b'NIBLEAF_PUBLIC_SEARCH_ENABLED' in failed.stderr + failed.stdout
    owner = {'Authorization': 'Bearer test-owner-token'}
    original_mode = sql(f'SELECT "accessMode" FROM project WHERE id={quoted(PROJECT)}')
    raw = secrets.token_bytes(32)
    native_token = base64.urlsafe_b64encode(raw).decode().rstrip('=')
    native_id = str(uuid.uuid4())
    sql(f"INSERT INTO rust_identity_session(id,user_id,token_hash,csrf_secret_hash,expires_at) VALUES ({quoted(native_id)},'test-owner',decode('{hashlib.sha256(raw).hexdigest()}','hex'),decode('{hashlib.sha256(b'csrf').hexdigest()}','hex'),NOW()+interval '1 hour')")
    native = {'Cookie': 'nibleaf_identity=' + native_token}
    with LOG.open('w') as log:
        api = subprocess.Popen([str(RUST/'target/debug/nibleaf-api')], env=ENV, stdout=log, stderr=log)
        try:
            wait_ready(api)
            mode('PUBLIC')
            value = request(PATH+'?q=scopeprobe&limit=50')['data']
            assert set(value) == {'hits','runtime','capabilities'}
            assert value['runtime']=='tantivy' and value['capabilities']=={'answer':False}
            assert value['hits'] and all(hit['language']!='fr' for hit in value['hits'])
            assert all(set(hit) <= {'id','title','path','description','icon','snippet','score','language'} for hit in value['hits'])
            for query in ['', '?q=', '?q=++', '?q='+urllib.parse.quote('界'*200), '?q='+urllib.parse.quote('😀'*100)]:
                request(PATH+query)
            for query in ['q=x&q=y', 'q=x&allowed_page_ids=foreign', 'q=x&limit=NaN', 'q=x&limit=2.1',
                          'q='+urllib.parse.quote('界'*201), 'q='+urllib.parse.quote('😀'*101)]:
                request(PATH+'?'+query,422)
            request(PATH,405,method='POST')
            request('/v1/public/unknown',404)
            request(PATH+'?q=privacypublicquerysentinel', headers={'Cookie':'unrelated=privacypubliccookiesentinel'})
            mode('WORKSPACE')
            request(PATH+'?q=scopeprobe',404)
            request(PATH+'?q=x&q=y',404) # admission concealment precedes invalid input
            request(PATH+'?q=scopeprobe',headers=owner)
            request(PATH+'?q=scopeprobe',headers={'Cookie':'nibleaf_session=test-owner-token'})
            request(PATH+'?q=scopeprobe',headers=native)
            request(PATH+'?q=scopeprobe',404,headers={'Authorization':'Bearer test-unrelated-token'})
            for headers in [ {'Cookie':'nibleaf_identity=invalid'}, {'Cookie':'nibleaf_identity'},
                {'Authorization':'Bearer invalid'}, {'Cookie':native['Cookie'],'Authorization':'Bearer test-owner-token'},
                {'Cookie':'nibleaf_session=test-owner-token','Authorization':'Bearer test-owner-token'},
                {'Cookie':'nibleaf_session=test-owner-token; nibleaf_session=test-owner-token'} ]:
                request(PATH+'?q=scopeprobe',401,headers=headers)
            sql(f"UPDATE rust_identity_session SET revoked_at=NOW(),revoke_reason='public_proof' WHERE id={quoted(native_id)}")
            request(PATH+'?q=scopeprobe',401,headers=native)
            sql(f"UPDATE rust_identity_session SET revoked_at=NULL,revoke_reason=NULL WHERE id={quoted(native_id)}")
            reader, audience, session, grant = (str(uuid.uuid4()) for _ in range(4))
            reader_token = secrets.token_urlsafe(32)
            page = 'search-en-'+PROJECT
            sql(f'''INSERT INTO reader(id,"projectId",email,status,"createdAt","updatedAt") VALUES ({quoted(reader)},{quoted(PROJECT)},'public-proof@example.test','ACTIVE',NOW(),NOW());
                INSERT INTO audience(id,"projectId",name,"createdAt","updatedAt") VALUES ({quoted(audience)},{quoted(PROJECT)},'Public proof',NOW(),NOW());
                INSERT INTO reader_audience("readerId","audienceId","createdAt") VALUES ({quoted(reader)},{quoted(audience)},NOW());
                INSERT INTO audience_grant(id,"projectId","audienceId","pageId","scopeKey","createdAt") VALUES ({quoted(grant)},{quoted(PROJECT)},{quoted(audience)},{quoted(page)},{quoted('PAGE:'+page)},NOW());
                INSERT INTO reader_session(id,"projectId","readerId","tokenHash",source,"expiresAt","lastUsedAt","createdAt") VALUES ({quoted(session)},{quoted(PROJECT)},{quoted(reader)},'{hashlib.sha256(reader_token.encode()).hexdigest()}','INVITATION',NOW()+interval '1 hour',NOW()-interval '10 minutes',NOW());''')
            cookie = {'Cookie': f'nibleaf_reader_{PROJECT}={reader_token}'}
            before = sql(f'SELECT "lastUsedAt" FROM reader_session WHERE id={quoted(session)}')
            mode('READERS')
            request(PATH+'?q=scopeprobe',404)
            hits = request(PATH+'?q=scopeprobe',headers=cookie)['data']['hits']
            assert [hit['id'] for hit in hits] == [page]
            request(PATH+'?q=scopeprobe',headers=owner) # live workspace override
            sql(f'DELETE FROM audience_grant WHERE id={quoted(grant)}')
            assert request(PATH+'?q=scopeprobe',headers=cookie)['data']['hits']==[]
            sql(f'UPDATE reader_session SET "revokedAt"=NOW() WHERE id={quoted(session)}')
            request(PATH+'?q=scopeprobe',404,headers=cookie)
            assert before == sql(f'SELECT "lastUsedAt" FROM reader_session WHERE id={quoted(session)}')
            mode('PUBLIC')
            sql(f'UPDATE project SET "takedownAt"=NOW() WHERE id={quoted(PROJECT)}')
            request(PATH+'?q=scopeprobe',404)
            sql(f'UPDATE project SET "takedownAt"=NULL WHERE id={quoted(PROJECT)}')
            # Lock two real server selections; the third is immediate Busy and
            # all public errors, including deadlines, carry credential privacy.
            lock = subprocess.Popen(['psql',DB,'-XqAt','-v','ON_ERROR_STOP=1','-c',f"BEGIN; LOCK rust_search_generation IN ACCESS EXCLUSIVE MODE; SELECT id FROM rust_identity_session WHERE id={quoted(native_id)} FOR UPDATE; SELECT pg_sleep(4); COMMIT"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                for _ in range(60):
                    if sql("SELECT EXISTS(SELECT 1 FROM pg_locks l JOIN pg_class c ON c.oid=l.relation WHERE c.relname='rust_search_generation' AND l.mode='AccessExclusiveLock' AND l.granted)")=='t': break
                    time.sleep(.025)
                else: raise AssertionError('lock barrier absent')
                with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                    active = [pool.submit(request,PATH+'?q=scopeprobe',503) for _ in range(2)]
                    for _ in range(60):
                        count = int(sql("SELECT count(*) FROM pg_stat_activity WHERE wait_event_type='Lock' AND query LIKE '%nibleaf:published-search-selection%'"))
                        if count==2: break
                        time.sleep(.01)
                    else: raise AssertionError('owned server barrier absent')
                    started=time.monotonic(); request(PATH+'?q=scopeprobe',503,headers=native)
                    assert time.monotonic()-started<.5
                    for pending in active: pending.result()
            finally:
                lock.wait(timeout=8)
            # Unavailable selected evidence never becomes older results, while
            # an unpublished site is concealed. All mutations are disposable.
            assert pathlib.Path(ROOT).resolve().is_relative_to(pathlib.Path('/tmp'))
            generation = sql(f"SELECT generation_id FROM rust_search_generation WHERE project_id={quoted(PROJECT)} AND state='ready'")
            manifest = pathlib.Path(ROOT)/'projects'/hashlib.sha256(PROJECT.encode()).hexdigest()/'generations'/generation/'manifest.json'
            saved = manifest.with_name('manifest.'+uuid.uuid4().hex+'.held')
            manifest.rename(saved)
            try: request(PATH+'?q=scopeprobe',503)
            finally: saved.rename(manifest)
            deployment = sql(f'SELECT id FROM deployment WHERE "projectId"={quoted(PROJECT)} AND status=\'READY\' ORDER BY version DESC LIMIT 1')
            sql(f"UPDATE deployment SET status='FAILED' WHERE id={quoted(deployment)}")
            try: request(PATH+'?q=scopeprobe',404)
            finally: sql(f"UPDATE deployment SET status='READY' WHERE id={quoted(deployment)}")
            import runpy
            runpy.run_path(str(RUST/'scripts/native_authority_proof.py'))['verify'](ENV,RUST,PROJECT,native,sql,native_id)
            runpy.run_path(str(RUST/'scripts/test-search-network-drain.py'))['run'](ENV,RUST,PROJECT,sql)
            metrics = urllib.request.urlopen(urllib.request.Request(BASE+'/metrics', headers={'Authorization':'Bearer public-proof-metrics'})).read().decode()
            assert 'nibleaf_public_search_responses_total' in metrics
            assert PROJECT not in metrics and 'privacypublic' not in metrics
            pathlib.Path('/tmp/nibleaf-public-search-metrics.prom').write_text(metrics)
        finally:
            mode(original_mode)
            api.terminate()
            try: api.wait(timeout=10)
            except subprocess.TimeoutExpired: api.kill(); api.wait(); raise
    text=LOG.read_text()
    assert '"level":"DEBUG"' in text and 'privacypublic' not in text and native_token not in text and reader_token not in text
    assert 'API HTTP and search work drained' in text
    print(json.dumps({'public_http_contract_checks':CHECKS,'access_modes':['PUBLIC','WORKSPACE','READERS'],
        'native_and_bridge_read_only_identity':True,'actual_busy_and_deadline_privacy':True,'reader_heartbeat_unchanged':True,
        'debug_sentinels_absent':True,'production_routing_changed':False}))
if __name__=='__main__': main()
