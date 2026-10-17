#!/usr/bin/env python3
"""Finite native worker/API S3 wire, failure, cold cache and retirement proof.
Strictly local disposable PostgreSQL and private local fixture roots; not AWS,
object-lock/versioning/durability or physical multi-host qualification.
"""
import base64, concurrent.futures, hashlib, json, math, os, pathlib, re, secrets, subprocess, tempfile, time, urllib.parse, urllib.request, urllib.error, uuid
from s3_protocol_fixture import Fixture, ACCESS, SECRET, BUCKET
DB=os.environ['NIBLEAF_TEST_DATABASE_URL'];parsed=urllib.parse.urlsplit(DB)
assert parsed.hostname in {'localhost','127.0.0.1'} and parsed.path=='/nibleaf_rust_test'
RUST=pathlib.Path(__file__).resolve().parents[1];BIN=pathlib.Path(os.environ.get('NIBLEAF_OBJECT_TEST_BIN_DIR',str(RUST/'target/debug')))
LOAD_DOCUMENTS=int(os.environ.get('NIBLEAF_OBJECT_LOAD_DOCUMENTS','0'));assert LOAD_DOCUMENTS in {0,5000}
EVIDENCE=pathlib.Path(os.environ.get('NIBLEAF_OBJECT_HTTP_EVIDENCE','/home/user/verification/distribution-http'))
EVIDENCE.mkdir(mode=0o700,parents=True,exist_ok=True)

def sql(statement):return subprocess.check_output(['psql',DB,'-XqAt','-v','ON_ERROR_STOP=1','-c',statement],text=True,timeout=15).strip()
def q(value):return "'"+value.replace("'","''")+"'"
def wait_for(predicate,message,seconds=35):
    until=time.monotonic()+seconds
    while time.monotonic()<until:
        if predicate():return
        time.sleep(.05)
    raise AssertionError(message)
def get_count(fixture):return sum(r['kind']=='get' for r in fixture.snapshot())

class Proof:
    def __init__(self,folder,fixture,endpoint):
        self.folder=folder;self.fixture=fixture;self.api={};self.logs={};self.worker=None;self.count=0
        self.measurements={};self.replicas=[str(uuid.uuid4()),str(uuid.uuid4()),str(uuid.uuid4())]
        raw=secrets.token_bytes(32);csrf=secrets.token_bytes(32);self.session=str(uuid.uuid4())
        self.native=base64.urlsafe_b64encode(raw).decode().rstrip('=');self.csrf=base64.urlsafe_b64encode(csrf).decode().rstrip('=')
        sql(f"INSERT INTO rust_identity_session(id,user_id,token_hash,csrf_secret_hash,expires_at) VALUES('{self.session}','test-owner',decode('{hashlib.sha256(raw).hexdigest()}','hex'),decode('{hashlib.sha256(csrf).hexdigest()}','hex'),NOW()+interval '1 hour')")
        self.headers={'Cookie':'nibleaf_identity='+self.native,'x-csrf-token':self.csrf,'Origin':'http://localhost:4310','Content-Type':'application/json'}
        self.env={**os.environ,'NIBLEAF_DATABASE_URL':DB,'NIBLEAF_AUTHORITY':'native','NIBLEAF_NATIVE_IDENTITY_ENABLED':'true','NIBLEAF_PUBLIC_SEARCH_ENABLED':'true',
          'NIBLEAF_METRICS_BEARER_TOKEN':'distribution-proof-metrics','NIBLEAF_TRUSTED_ORIGINS':'http://localhost:4310','NIBLEAF_SEARCH_DISTRIBUTION':'s3',
          'NIBLEAF_SEARCH_ARTIFACT_S3_ENDPOINT':endpoint,'NIBLEAF_SEARCH_ARTIFACT_S3_REGION':'us-east-1','NIBLEAF_SEARCH_ARTIFACT_S3_BUCKET':BUCKET,
          'NIBLEAF_SEARCH_ARTIFACT_S3_ACCESS_KEY_ID':ACCESS,'NIBLEAF_SEARCH_ARTIFACT_S3_SECRET_ACCESS_KEY':SECRET,'NIBLEAF_SEARCH_ARTIFACT_ALLOW_HTTP':'true',
          'NIBLEAF_SEARCH_ARTIFACT_TIMEOUT_MS':'3000','NIBLEAF_SEARCH_QUERY_TIMEOUT_MS':'5000','NIBLEAF_SEARCH_QUERY_CONCURRENCY':'2',
          'NIBLEAF_SEARCH_CACHE_ENTRIES':'1','NIBLEAF_SEARCH_CACHE_BYTES':'67108864','NIBLEAF_SEARCH_CACHE_ENTRY_BYTES':'33554432',
          'NIBLEAF_SEARCH_INDEX_ROOT':str(folder/'worker-build'),'NIBLEAF_WORKER_CONCURRENCY':'2',
          'NIBLEAF_INVITATION_SIGNING_KEY':'MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=',
          'NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY':'ZmVkY2JhOTg3NjU0MzIxMGZlZGNiYTk4NzY1NDMyMTA=',
          'AWS_ACCESS_KEY_ID':'ambient-hostile-access','AWS_SECRET_ACCESS_KEY':'ambient-hostile-secret','RUST_LOG':'info,nibleaf_api=debug,object_store=debug'}
        for name in ['POSTMARK_API_KEY','NIBLEAF_POSTMARK_API_KEY','SMTP_URL','NIBLEAF_SMTP_URL']:self.env.pop(name,None)
    def base(self,index):return 'http://127.0.0.1:'+str(4341+index if index<2 else 4344)
    def start_api(self,index):
        log=(EVIDENCE/f'api-{index}-{time.time_ns()}.log').open('w');self.logs[str(log.name)]=log
        env={**self.env,'NIBLEAF_BIND_ADDR':'127.0.0.1:'+str(4341+index if index<2 else 4344),'NIBLEAF_SEARCH_QUERY_CACHE_ROOT':str(self.folder/f'cache-{index}'),'NIBLEAF_SEARCH_REPLICA_ID':self.replicas[index]}
        if index==2:env.update({'NIBLEAF_SEARCH_QUERY_TIMEOUT_MS':'500','NIBLEAF_SEARCH_QUERY_CONCURRENCY':'1'})
        self.api[index]=subprocess.Popen([str(BIN/'nibleaf-api')],env=env,stdout=log,stderr=log)
        def ready():
            assert self.api[index].poll() is None,'object API exited; inspect fixture logs'
            try:urllib.request.urlopen(self.base(index)+'/health/ready',timeout=.3).close();return True
            except OSError:return False
        wait_for(ready,'object API not ready',20)
    def stop_api(self,index,force=False):
        process=self.api.pop(index,None)
        if process is None:return
        if process.poll() is None:process.kill() if force else process.terminate()
        try:process.wait(timeout=12)
        except subprocess.TimeoutExpired:process.kill();process.wait();raise
        assert process.returncode==(-9 if force else 0)
    def start_worker(self):
        log=(EVIDENCE/f'worker-{time.time_ns()}.log').open('w');self.logs[str(log.name)]=log
        self.worker=subprocess.Popen([str(BIN/'nibleaf-worker')],env={**self.env,'NIBLEAF_WORKER_BIND_ADDR':'127.0.0.1:4343'},stdout=log,stderr=log)
        def ready():
            assert self.worker.poll() is None,'object worker exited; inspect fixture logs'
            try:urllib.request.urlopen('http://127.0.0.1:4343/health/ready',timeout=.3).close();return True
            except OSError:return False
        wait_for(ready,'object worker not ready',20)
    def request(self,index,path,code=200,body=None,native=False,headers=None,method=None):
        request=urllib.request.Request(self.base(index)+path,data=json.dumps(body).encode() if body is not None else None,
          headers=headers if headers is not None else self.headers if native else {},method=method or ('POST' if body is not None else 'GET'))
        try:response=urllib.request.urlopen(request,timeout=12)
        except urllib.error.HTTPError as error:response=error
        data=json.loads(response.read());assert response.status==code,(path.split('?')[0],response.status,code,data)
        if '/public/' in path:
            assert 'no-store' in response.headers['Cache-Control'];assert 'cookie' in response.headers['Vary'].lower()
        self.count+=1;return data
    def query(self,index,term='remotecommon',code=200,headers=None):return self.request(index,f'/v1/public/sites/{self.project}/search?q={term}&limit=50',code,headers=headers)
    def close(self):
        for index in list(self.api):self.stop_api(index)
        if self.worker is not None:
            if self.worker.poll() is None:self.worker.terminate()
            try:self.worker.wait(timeout=12)
            except subprocess.TimeoutExpired:self.worker.kill();self.worker.wait();raise
            assert self.worker.returncode==0
        for file in self.logs.values():file.close()
        for path in self.logs:
            text=pathlib.Path(path).read_text()
            assert SECRET not in text and ACCESS not in text and self.native not in text and self.csrf not in text and 'private-query-sentinel' not in text,'credential/query sentinel appeared in DEBUG logs'

def invalid_startup_settings_fail_closed(proof):
    base={**proof.env,'NIBLEAF_SEARCH_QUERY_CACHE_ROOT':str(proof.folder/'invalid-config-root'),'NIBLEAF_SEARCH_REPLICA_ID':str(uuid.uuid4()),'NIBLEAF_BIND_ADDR':'127.0.0.1:4348'}
    cases=[('NIBLEAF_SEARCH_DISTRIBUTION','shadow'),('NIBLEAF_SEARCH_DISTRIBUTION',''),
      ('NIBLEAF_SEARCH_ARTIFACT_ALLOW_HTTP','false'),('NIBLEAF_SEARCH_ARTIFACT_MAX_BYTES','0'),
      ('NIBLEAF_SEARCH_QUERY_CACHE_ROOT','relative-root'),('NIBLEAF_SEARCH_REPLICA_ID',str(uuid.UUID(int=0))),
      ('NIBLEAF_SEARCH_CACHE_ENTRIES','0'),('NIBLEAF_SEARCH_ARTIFACT_S3_ENDPOINT','http://private-config-sentinel:password@127.0.0.1/')]
    for setting,value in cases:
        result=subprocess.run([str(BIN/'nibleaf-api')],env={**base,setting:value},capture_output=True,timeout=10)
        assert result.returncode==1
        assert b'private-config-sentinel' not in result.stdout+result.stderr and b'password@' not in result.stdout+result.stderr
    unset={**base};unset.pop('NIBLEAF_SEARCH_DISTRIBUTION')
    result=subprocess.run([str(BIN/'nibleaf-api')],env=unset,capture_output=True,timeout=10);assert result.returncode==1
    result=subprocess.run([str(BIN/'nibleaf-worker')],env={**base,'NIBLEAF_WORKER_CONCURRENCY':'64'},capture_output=True,timeout=10);assert result.returncode==1
    cache_only={key:value for key,value in base.items() if not key.startswith('NIBLEAF_SEARCH_ARTIFACT_')}
    cache_only.pop('NIBLEAF_SEARCH_DISTRIBUTION')
    result=subprocess.run([str(BIN/'nibleaf-api')],env=cache_only,capture_output=True,timeout=10);assert result.returncode==1
    return len(cases)+3

def publication(proof):
    value=proof.request(0,f'/v1/projects/{proof.project}/publications',202,{},True)['data'];return value['id']
def ready(deployment):return sql(f"SELECT status FROM deployment WHERE id={q(deployment)}")=='READY'
def add_page(proof,slug,marker):
    return proof.request(0,f'/v1/projects/{proof.project}/pages',201,{'language_id':proof.language,'title':marker,'slug':slug,'content':marker+' remotecommon immutable text'},True)['data']['id']

def publication_and_cold(proof):
    proof.start_api(0);proof.start_api(1)
    proof.project=proof.request(0,'/v1/projects',201,{'name':'Object distribution proof'},True)['data']['id'];uuid.UUID(proof.project)
    proof.language=sql(f'SELECT id FROM language WHERE "projectId"={q(proof.project)} AND "isDefault"=TRUE')
    proof.page=add_page(proof,'one','remoteone')
    if LOAD_DOCUMENTS:
        sql(f"""INSERT INTO page(id,"projectId","languageId","branchId",kind,title,slug,path,content,hidden,position,revision,"createdAt","updatedAt")
          SELECT 'object-load-'||n||'-'||{q(proof.project)},{q(proof.project)},{q(proof.language)},b.id,'PAGE','Load support '||n,
             'load-support-'||n,'load-support-'||n,repeat('loadsupport immutable corpus ordinary text ',16),FALSE,n,0,NOW(),NOW()
          FROM generate_series(1,{LOAD_DOCUMENTS-1}) n CROSS JOIN branch b WHERE b."projectId"={q(proof.project)} AND b."isDefault"=TRUE""")

    proof.request(0,f'/v1/projects/{proof.project}/publications',422,{'delivery_kind':'local'},True)
    deployment=publication(proof);proof.start_worker()
    wait_for(lambda:sql(f"SELECT count(*) FROM rust_search_artifact_upload u JOIN rust_search_generation g ON g.generation_id=u.generation_id WHERE g.project_id={q(proof.project)} AND u.state='unknown'")=='1','ambiguous upload not durably recorded')
    assert not ready(deployment)
    assert sql(f"SELECT count(*) FROM rust_search_artifact WHERE project_id={q(proof.project)}")=='0'
    proof.query(0,code=404)
    proof.fixture.hold_get=False
    wait_for(lambda:ready(deployment),'publication did not wait/recover conditional upload',45)
    assert sql(f"SELECT rust_search_delivery_mode FROM deployment WHERE id={q(deployment)}")=='object'
    assert sql(f"SELECT state||':'||delivery_kind FROM rust_search_generation WHERE deployment_id={q(deployment)}")=='ready:object'
    assert sql(f"SELECT count(*) FROM rust_search_artifact WHERE deployment_id={q(deployment)}")=='1'
    sql(f'UPDATE project SET "accessMode"=\'WORKSPACE\' WHERE id={q(proof.project)}')
    before=get_count(proof.fixture);proof.query(0,code=404);assert get_count(proof.fixture)==before
    sql(f'UPDATE project SET "accessMode"=\'PUBLIC\' WHERE id={q(proof.project)}')
    proof.fixture.get_delay=.25;before=get_count(proof.fixture)
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        pending=[pool.submit(proof.query,0) for _ in range(2)]
        for job in pending:assert len(job.result()['data']['hits'])==1
    assert get_count(proof.fixture)-before==1,'duplicate cold download, not single-flight'
    proof.fixture.get_delay=0
    assert len(proof.query(1)['data']['hits'])==1
    if LOAD_DOCUMENTS:
        def timed(_):
            started=time.monotonic();assert len(proof.query(0)['data']['hits'])==1;return (time.monotonic()-started)*1000
        sequential=[timed(0) for _ in range(10)]
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:parallel=list(pool.map(timed,range(10)))
        def percentiles(values):
            values=sorted(values);return {name:round(values[math.ceil(q*len(values))-1],3) for name,q in [('p50',.5),('p95',.95),('p99',.99)]}
        proof.measurements={'documents':LOAD_DOCUMENTS,'sequential_10_ms':percentiles(sequential),'concurrent_2_10_ms':percentiles(parallel),'slo_approved':False}
    return deployment

def corruption_revocation_and_reader(proof,deployment):
    key=sql(f'SELECT object_key FROM rust_search_artifact WHERE deployment_id={q(deployment)}')
    with proof.fixture.lock:
        original=proof.fixture.objects[key];changed=bytearray(original);changed[len(changed)//2]^=1;proof.fixture.objects[key]=bytes(changed)
    proof.stop_api(1);proof.start_api(1)
    assert len(proof.query(0)['data']['hits'])==1
    proof.query(1,code=503) # Master/local files exist, but remote mode never falls back.
    with proof.fixture.lock:proof.fixture.objects[key]=original
    assert len(proof.query(1)['data']['hits'])==1
    sql(f'UPDATE project SET "accessMode"=\'WORKSPACE\' WHERE id={q(proof.project)}')
    for index in [0,1]:assert len(proof.query(index,headers=proof.headers)['data']['hits'])==1
    sql(f"UPDATE rust_identity_session SET revoked_at=NOW(),revoke_reason='object_fixture' WHERE id={q(proof.session)}")
    for index in [0,1]:proof.query(index,code=401,headers=proof.headers)
    sql(f"UPDATE rust_identity_session SET revoked_at=NULL,revoke_reason=NULL WHERE id={q(proof.session)}")
    reader,audience,session,grant=(str(uuid.uuid4()) for _ in range(4));token=secrets.token_urlsafe(32)
    sql(f'''INSERT INTO reader(id,"projectId",email,status,"createdAt","updatedAt") VALUES({q(reader)},{q(proof.project)},'object-reader@example.test','ACTIVE',NOW(),NOW());
      INSERT INTO audience(id,"projectId",name,"createdAt","updatedAt") VALUES({q(audience)},{q(proof.project)},'Object reader',NOW(),NOW());
      INSERT INTO reader_audience("readerId","audienceId","createdAt") VALUES({q(reader)},{q(audience)},NOW());
      INSERT INTO audience_grant(id,"projectId","audienceId","pageId","scopeKey","createdAt") VALUES({q(grant)},{q(proof.project)},{q(audience)},{q(proof.page)},{q('PAGE:'+proof.page)},NOW());
      INSERT INTO reader_session(id,"projectId","readerId","tokenHash",source,"expiresAt","lastUsedAt","createdAt") VALUES({q(session)},{q(proof.project)},{q(reader)},'{hashlib.sha256(token.encode()).hexdigest()}','INVITATION',NOW()+interval '1 hour',NOW()-interval '10 minutes',NOW());''')
    sql(f'UPDATE project SET "accessMode"=\'READERS\' WHERE id={q(proof.project)}')
    cookie={'Cookie':f'nibleaf_reader_{proof.project}={token}'}
    before=sql(f'SELECT "lastUsedAt" FROM reader_session WHERE id={q(session)}')
    for index in [0,1]:assert [h['id'] for h in proof.query(index,headers=cookie)['data']['hits']]==[proof.page]
    sql(f'DELETE FROM audience_grant WHERE id={q(grant)}')
    for index in [0,1]:assert proof.query(index,headers=cookie)['data']['hits']==[]
    sql(f'UPDATE reader_session SET "revokedAt"=NOW() WHERE id={q(session)}')
    for index in [0,1]:proof.query(index,code=404,headers=cookie)
    assert before==sql(f'SELECT "lastUsedAt" FROM reader_session WHERE id={q(session)}')
    sql(f'UPDATE project SET "accessMode"=\'PUBLIC\' WHERE id={q(proof.project)}')

def eviction_and_crash(proof):
    add_page(proof,'two','remotetwo');proof.fixture.block_put=True
    second=publication(proof)
    wait_for(lambda:sql(f"SELECT count(*) FROM rust_search_artifact_upload WHERE project_id={q(proof.project)}")=='2','second upload scope missing')
    assert not ready(second)
    assert len(proof.query(0,'remoteone')['data']['hits'])==1
    proof.fixture.block_put=False
    wait_for(lambda:ready(second),'second object publication not ready',45)
    for index in [0,1]:assert len(proof.query(index)['data']['hits'])==2
    for index in [0,1]:
        assert len(list((proof.folder/f'cache-{index}').glob('entry-*')))==1
        assert sql(f"SELECT count(*) FROM rust_search_materialization WHERE replica_id={q(proof.replicas[index])} AND state='purged'")!='0'
    before=sql(f"SELECT materialization_id FROM rust_search_materialization WHERE replica_id={q(proof.replicas[1])} AND state='ready'")
    proof.stop_api(1,force=True)
    assert sql(f"SELECT state FROM rust_search_materialization WHERE materialization_id={q(before)}")=='ready','TTL/process exit falsely acknowledged purge'
    proof.start_api(1)
    assert sql(f"SELECT state FROM rust_search_materialization WHERE materialization_id={q(before)}")=='purged'
    assert len(list((proof.folder/'cache-1').glob('entry-*')))==0
    assert len(proof.query(1)['data']['hits'])==2
    return second

def accepted_worker_upload_is_drained(proof):
    before=sum(row['kind']=='accepted' for row in proof.fixture.snapshot())
    previous_get=get_count(proof.fixture);proof.fixture.get_delay=1.2
    deployment=publication(proof)
    wait_for(lambda:sum(row['kind']=='accepted' for row in proof.fixture.snapshot())>before and get_count(proof.fixture)>previous_get,'accepted upload verification barrier absent')
    process=proof.worker;process.terminate()
    try:process.wait(timeout=.15)
    except subprocess.TimeoutExpired:pass
    else:raise AssertionError('worker dropped accepted provider verification')
    process.wait(timeout=10);assert process.returncode==0
    assert sql(f"SELECT count(*) FROM rust_search_artifact WHERE deployment_id={q(deployment)}")=='1'
    assert sql(f"SELECT state FROM outbox_event WHERE event_type='search.generation.requested' AND aggregate_id={q(deployment)}")=='delivered'
    assert len(list((proof.folder/'worker-build'/'.object-spool').iterdir()))==0
    proof.fixture.get_delay=0;proof.start_worker();wait_for(lambda:ready(deployment),'publication did not recover after drained worker restart',45)
    assert len(proof.query(0)['data']['hits'])==2

def accepted_storage_deadline_is_owned(proof):
    proof.fixture.get_delay=1.5
    proof.start_api(2)
    before=get_count(proof.fixture)
    proof.query(2,code=503)
    assert get_count(proof.fixture)>before,'cold transfer was not active before deadline' 
    started=time.monotonic();proof.query(2,code=503)
    assert time.monotonic()-started<.15,'abandoned download released logical capacity'
    process=proof.api[2];process.terminate()
    try:process.wait(timeout=.15)
    except subprocess.TimeoutExpired:pass
    else:raise AssertionError('shutdown dropped accepted object transfer')
    process.wait(timeout=8);assert process.returncode==0
    proof.stop_api(2)
    proof.fixture.get_delay=0
    assert len(list((proof.folder/'cache-2').glob('entry-*')))==0
    assert sql(f"SELECT count(*) FROM rust_search_materialization WHERE replica_id={q(proof.replicas[2])} AND state!='purged'")=='0'

def main():
    assert sql("SELECT count(*) FROM pg_stat_activity WHERE application_name='nibleaf-object-fixture'")=='0'
    disk=pathlib.Path('/home/user/.cache/nibleaf-object-fixtures');disk.mkdir(mode=0o700,parents=True,exist_ok=True)
    fixture=Fixture();endpoint=fixture.start()
    try:
        with tempfile.TemporaryDirectory(prefix='object-',dir=disk) as temp:
            proof=Proof(pathlib.Path(temp),fixture,endpoint)
            try:
                invalid=invalid_startup_settings_fail_closed(proof)
                first=publication_and_cold(proof)
                corruption_revocation_and_reader(proof,first)
                second=eviction_and_crash(proof)
                accepted_worker_upload_is_drained(proof)
                accepted_storage_deadline_is_owned(proof)
            finally:proof.close()
            for replica in proof.replicas:
                assert sql(f"SELECT count(*) FROM rust_search_materialization WHERE replica_id={q(replica)} AND state!='purged'")=='0'
            requests=fixture.snapshot();assert fixture.auth_failures==0
            assert all(request.get('conditional') for request in requests if request['kind']=='put')
            assert sum(request['kind']=='accepted' for request in requests)==3
            result={'kind':'local_s3_wire_and_native_lifecycle_proof','http_assertions':proof.count,'invalid_startup_checks':invalid,'ambient_credentials_ignored':True,'aws4_signatures_verified':True,
              'conditional_put_only':True,'unique_accepted_objects':3,'accepted_worker_provider_io_drained':True,'ambiguous_upload_persisted_before_ready':True,'single_flight_cold_read':True,
              'deny_before_storage_io':True,'native_and_reader_revocation_fresh':True,'no_reader_heartbeat_write':True,
              'corrupt_object_no_local_fallback':True,'bounded_eviction':True,'crash_not_purge_then_owned_restart_cleanup':True,
              'measurements':proof.measurements,'normal_shutdown_catalog_purge':True,'accepted_storage_io_owned_after_deadline':True,'provider_durability_versioning_lock_qualified':False,'production_ready':False}
            (EVIDENCE/'result.json').write_text(json.dumps(result,indent=2)+'\n');print('object-http-proof '+json.dumps(result))
    finally:fixture.close()
if __name__=='__main__':main()
