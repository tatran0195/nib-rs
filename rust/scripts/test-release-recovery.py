#!/usr/bin/env python3
"""Local-only release binary / scoped DB+artifact recovery and HTTP load proof.

Uses an explicitly disposable loopback source and a NEW uniquely named restore
DB. Never drops/migrates/updates the source or a pre-existing target database.
This is one scoped fixture artifact plus whole fixture DB, not a qualification
of real production backups, object-store durability or multi-host deployment.
"""
import base64, concurrent.futures, hashlib, json, math, os, pathlib, re, secrets, subprocess, sys, time, urllib.parse, urllib.request, urllib.error, uuid

RUST=pathlib.Path(__file__).resolve().parents[1]
SOURCE=os.environ['NIBLEAF_TEST_DATABASE_URL']; parsed=urllib.parse.urlsplit(SOURCE)
assert parsed.hostname in {'127.0.0.1','localhost'} and parsed.path=='/nibleaf_rust_test'
assert parsed.username=='nibleaf_test'
OUT=pathlib.Path(os.environ.get('NIBLEAF_RELEASE_EVIDENCE','/home/user/verification/production-release-recovery'))
OUT.mkdir(mode=0o700,parents=True,exist_ok=True)
BIN=RUST/'target/release'

def sql(database,statement):
    return subprocess.check_output(['psql',database,'-XqAt','-v','ON_ERROR_STOP=1','-c',statement],text=True,timeout=20).strip()
def run(arguments,**options):
    return subprocess.run(arguments,check=True,capture_output=True,text=True,timeout=options.pop('timeout',60),**options)
def sha(path):
    digest=hashlib.sha256()
    with path.open('rb') as handle:
        for chunk in iter(lambda:handle.read(65536),b''): digest.update(chunk)
    return digest.hexdigest()
def request(base,path,headers=None):
    begin=time.monotonic()
    try: response=urllib.request.urlopen(urllib.request.Request(base+path,headers=headers or {}),timeout=10)
    except urllib.error.HTTPError as error: response=error
    body=json.loads(response.read())
    assert 'no-store' in response.headers.get('Cache-Control','') if '/public/' in path else True
    return response.status,body,(time.monotonic()-begin)*1000

def percentiles(values):
    values=sorted(values)
    return {name:round(values[max(0,math.ceil(percentile*len(values))-1)],3) for name,percentile in [('p50',.5),('p95',.95),('p99',.99)]}

def worker_schema_smoke(folder, tag, environment):
    schema=folder/'worker-schema.dump'
    run(['pg_dump',SOURCE,'--format=custom','--schema-only','--no-owner','--no-acl','--file',str(schema)])
    name='nibleaf_rust_test_worker_'+tag
    assert re.fullmatch(r'nibleaf_rust_test_worker_[a-f0-9]{12}',name)
    run(['sudo','-u','postgres','createdb','--owner=nibleaf_test',name])
    target=urllib.parse.urlunsplit(parsed._replace(path='/'+name))
    run(['pg_restore','--dbname',target,'--no-owner','--no-acl','--exit-on-error',str(schema)])
    assert sql(target,'SELECT count(*) FROM outbox_event')=='0'
    selected={**environment,'NIBLEAF_DATABASE_URL':target,'NIBLEAF_WORKER_BIND_ADDR':'127.0.0.1:4324','NIBLEAF_WORKER_CONCURRENCY':'1'}
    for key in ['POSTMARK_API_KEY','NIBLEAF_POSTMARK_API_KEY','SMTP_URL','NIBLEAF_SMTP_URL']:selected.pop(key,None)
    log=folder/'worker.log'
    with log.open('w') as output:
        child=subprocess.Popen([str(BIN/'nibleaf-worker')],env=selected,stdout=output,stderr=output)
        try:
            for _ in range(150):
                assert child.poll() is None
                try:urllib.request.urlopen('http://127.0.0.1:4324/health/ready',timeout=.3).close();break
                except OSError:time.sleep(.05)
            else:raise AssertionError('release worker not ready')
        finally:
            if child.poll() is None:child.terminate()
            try:child.wait(timeout=12)
            except subprocess.TimeoutExpired:child.kill();child.wait();raise
    assert child.returncode==0 and 'worker HTTP and accepted work drained' in log.read_text()
    assert sql(target,'SELECT count(*) FROM outbox_event')=='0'
    return {'schema_only_database':name,'ready_and_drained':True,'provider_or_outbox_replay':False}

def main():
    for binary in ['nibleaf-api','nibleaf-worker','search-ops']: assert (BIN/binary).is_file()
    tag=uuid.uuid4().hex[:12]; folder=OUT/tag;folder.mkdir(mode=0o700)
    metadata_raw=sql(SOURCE,'''SELECT json_build_object('project',d."projectId",'deployment',d.id,'generation',g.generation_id,'documents',g.document_count)
        FROM deployment d JOIN rust_search_generation g ON g.deployment_id=d.id AND g.project_id=d."projectId"
        WHERE d.id LIKE 'load-deployment-%' AND d.status='READY' AND g.state='ready' ORDER BY d."createdAt" DESC LIMIT 1''')
    if not metadata_raw:raise RuntimeError('Run the non-skip release load/recovery fixture after the schema-preflight harness; it deliberately clears earlier registry fixtures.')
    metadata=json.loads(metadata_raw)
    project=metadata['project'];deployment=metadata['deployment'];generation=metadata['generation']
    assert metadata['documents']==5000 and all(re.fullmatch(r'[a-zA-Z0-9_-]+',str(value)) for value in [project,deployment,generation])
    source_root=folder/'source-root';archive=folder/'generation.tar.zst'
    source_env={**os.environ,'NIBLEAF_DATABASE_URL':SOURCE}
    run([str(BIN/'search-ops'),'rebuild-new-root',str(source_root),project,deployment],env=source_env)
    exported=run([str(BIN/'search-ops'),'export-artifact',str(source_root),project,deployment,generation,str(archive)])
    artifact=json.loads(exported.stdout);assert artifact['artifact_sha256']==sha(archive)
    tables=['project','deployment','rust_search_generation','outbox_event','onboarding_run','rust_identity_session','member_invitation','member_invitation_delivery']
    query='SELECT json_build_object('+','.join("'"+table+"',(SELECT count(*) FROM "+table+")" for table in tables)+')'
    before=json.loads(sql(SOURCE,query))
    dump=folder/'database.dump'
    run(['pg_dump',SOURCE,'--format=custom','--no-owner','--no-acl','--file',str(dump)],timeout=120)
    database='nibleaf_rust_test_restore_'+tag
    assert re.fullmatch(r'nibleaf_rust_test_restore_[a-f0-9]{12}',database)
    run(['sudo','-u','postgres','createdb','--owner=nibleaf_test',database])
    target=urllib.parse.urlunsplit(parsed._replace(path='/'+database))
    run(['pg_restore','--dbname',target,'--no-owner','--no-acl','--exit-on-error',str(dump)],timeout=120)
    after=json.loads(sql(target,query));assert before==after
    roots=[]
    for index in range(2):
        root=folder/f'replica-{index}'
        run([str(BIN/'search-ops'),'import-artifact',str(archive),str(root),project,deployment,generation,artifact['artifact_sha256']])
        run([str(BIN/'search-ops'),'verify-selected',str(root),project,deployment],env={**source_env,'NIBLEAF_DATABASE_URL':target})
        roots.append(root)
    # Seed only the explicitly NEW disposable restored DB for native-only access
    # checks; never alter the source account state, credentials or memberships.
    raw=secrets.token_bytes(32);token=base64.urlsafe_b64encode(raw).decode().rstrip('=');session=str(uuid.uuid4())
    sql(target,f'''UPDATE "user" SET "suspendedAt"=NULL,banned=FALSE WHERE id='test-owner';
       INSERT INTO member(id,"organizationId","userId",role,"createdAt") SELECT '{uuid.uuid4()}',"organizationId",'test-owner','admin',NOW() FROM project WHERE id='{project}';
       INSERT INTO rust_identity_session(id,user_id,token_hash,csrf_secret_hash,expires_at)
       VALUES ('{session}','test-owner',decode('{hashlib.sha256(raw).hexdigest()}','hex'),decode('{hashlib.sha256(b'csrf').hexdigest()}','hex'),NOW()+interval '1 hour');''')
    env={**os.environ,'NIBLEAF_DATABASE_URL':target,'NIBLEAF_AUTHORITY':'native','NIBLEAF_NATIVE_IDENTITY_ENABLED':'true','NIBLEAF_PUBLIC_SEARCH_ENABLED':'true',
       'NIBLEAF_METRICS_BEARER_TOKEN':'release-proof-metrics','NIBLEAF_SEARCH_QUERY_TIMEOUT_MS':'5000','NIBLEAF_SEARCH_QUERY_CONCURRENCY':'2',
       'NIBLEAF_INVITATION_SIGNING_KEY':'MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=',
       'NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY':'ZmVkY2JhOTg3NjU0MzIxMGZlZGNiYTk4NzY1NDMyMTA='}
    processes=[];logs=[];native={'Cookie':'nibleaf_identity='+token};bases=['http://127.0.0.1:4321','http://127.0.0.1:4322']
    path=f'/v1/public/sites/{project}/search?q=loadprobe&limit=12'
    measurements={}
    try:
        for index,root in enumerate(roots):
            log=(folder/f'api-{index}.log').open('w');logs.append(log)
            child=subprocess.Popen([str(BIN/'nibleaf-api')],env={**env,'NIBLEAF_SEARCH_INDEX_ROOT':str(root),'NIBLEAF_BIND_ADDR':f'127.0.0.1:{4321+index}'},stdout=log,stderr=log)
            processes.append(child)
            for _ in range(150):
                assert child.poll() is None
                try: urllib.request.urlopen(bases[index]+'/health/ready',timeout=.3).close();break
                except (OSError,urllib.error.URLError):time.sleep(.05)
            else:raise AssertionError('release API readiness failed')
            status,body,_=request(bases[index],path);assert status==200 and len(body['data']['hits'])==12
            assert body['data']['runtime']=='tantivy' and body['data']['capabilities']=={'answer':False}
            status,_,_=request(bases[index],'/v1/projects',{'Authorization':'Bearer test-owner-token'});assert status==401
            status,body,_=request(bases[index],'/v1/identity/session',native);assert status==200 and isinstance(body['data']['expires_at'],str)
        sequential=[]
        for _ in range(20):
            status,body,elapsed=request(bases[0],path);assert status==200 and len(body['data']['hits'])==12;sequential.append(elapsed)
        measurements['sequential_20_ms']=percentiles(sequential)
        concurrent_latencies=[]
        for _ in range(10):
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                results=list(pool.map(lambda _:request(bases[0],path),range(2)))
            for status,body,elapsed in results: assert status==200;concurrent_latencies.append(elapsed)
        measurements['concurrent_2_20_ms']=percentiles(concurrent_latencies)
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            burst=list(pool.map(lambda _:request(bases[0],path),range(8)))
        assert all(status in {200,503} for status,_,_ in burst)
        measurements['burst_8']={'completed':sum(status==200 for status,_,_ in burst),'busy_or_deadline':sum(status==503 for status,_,_ in burst)}
        manifest=roots[1]/'projects'/hashlib.sha256(project.encode()).hexdigest()/'generations'/generation/'manifest.json';held=manifest.with_suffix('.held');manifest.rename(held)
        try: assert request(bases[1],path)[0]==503 and request(bases[0],path)[0]==200
        finally: held.rename(manifest)
        sql(target, f"""UPDATE project SET "accessMode"='WORKSPACE' WHERE id='{project}'""")
        for base in bases: assert request(base,path,native)[0]==200
        sql(target,f"UPDATE rust_identity_session SET revoked_at=NOW(),revoke_reason='release_proof' WHERE id='{session}'")
        for base in bases: assert request(base,path,native)[0]==401
        measurements['api_high_water']=[next(line for line in pathlib.Path(f'/proc/{p.pid}/status').read_text().splitlines() if line.startswith('VmHWM:')) for p in processes]
    finally:
        for process in processes:
            if process.poll() is None:process.terminate()
        for process in processes:
            try:process.wait(timeout=12)
            except subprocess.TimeoutExpired:process.kill();process.wait();raise
        for log in logs:log.close()
    for index in range(2):assert 'API HTTP and search work drained' in (folder/f'api-{index}.log').read_text()
    worker=worker_schema_smoke(folder,tag,env)
    result={'kind':'local_scoped_release_recovery_proof','database_name':database,'source_database_modified':False,'new_restore_database_only':True,
        'fixture_table_counts':before,'backup_sha256':sha(dump),'artifact':artifact,'release_binaries':{name:sha(BIN/name) for name in ['nibleaf-api','nibleaf-worker','search-ops']},
        'documents':5000,'independent_roots':2,'source_registry_verified':True,'corruption_isolated':True,'fresh_revocation_on_both':True,
        'worker_release_smoke':worker,'measurements':measurements,'slo_approved':False,'production_backup_or_provider_qualified':False,'worker_replay_executed':False,
        'scope_limitation':'quiescent disposable fixture DB plus one selected generation archive; not all project volumes/providers/offline replicas or online production backup consistency'}
    (OUT/'result.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2))
if __name__=='__main__':main()
