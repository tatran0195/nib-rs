"""Finite native-only authority / RFC3339 wire proof on disposable fixtures."""
import datetime, hashlib, json, pathlib, subprocess, tempfile, time, urllib.request, urllib.error

def verify(env, rust, project, native, sql, native_session_id):
    assert 'nibleaf_rust_test' in env['NIBLEAF_DATABASE_URL']
    base='http://127.0.0.1:4317'
    selected={**env,'NIBLEAF_AUTHORITY':'native','NIBLEAF_BIND_ADDR':'127.0.0.1:4317'}
    for bad in [{'NIBLEAF_AUTHORITY':'legacy'},{'NIBLEAF_AUTHORITY':''},{'NIBLEAF_NATIVE_IDENTITY_ENABLED':'false'}]:
        result=subprocess.run([str(rust/'target/debug/nibleaf-api')],env={**selected,**bad},capture_output=True,timeout=10)
        assert result.returncode==1 and b'NIBLEAF_AUTHORITY' in result.stdout+result.stderr
    checks=0
    def request(path,code,headers=None):
        nonlocal checks
        try: response=urllib.request.urlopen(urllib.request.Request(base+path,headers=headers or {}),timeout=8)
        except urllib.error.HTTPError as error: response=error
        body=json.loads(response.read())
        assert response.status==code,(path,response.status,code)
        checks+=1
        return body
    # Materialize a separate replica from a verified portable envelope. The
    # second API is not given the first API's filesystem root.
    temp=tempfile.TemporaryDirectory(prefix='nibleaf-independent-replica-')
    folder=pathlib.Path(temp.name); replica=folder/'replica'; packed=folder/'generation.tar.zst'
    deployment,generation=sql(f"SELECT d.id||'|'||g.generation_id FROM deployment d JOIN rust_search_generation g ON g.deployment_id=d.id AND g.project_id=d.\"projectId\" WHERE d.\"projectId\"='{project}' AND d.status='READY' ORDER BY d.version DESC LIMIT 1").split('|')
    ops=str(rust/'target/debug/search-ops')
    exported=subprocess.run([ops,'export-artifact',env['NIBLEAF_SEARCH_INDEX_ROOT'],project,deployment,generation,str(packed)],capture_output=True,text=True,check=True,timeout=30)
    receipt=json.loads(exported.stdout)
    subprocess.run([ops,'import-artifact',str(packed),str(replica),project,deployment,generation,receipt['artifact_sha256']],capture_output=True,check=True,timeout=30)
    subprocess.run([ops,'verify-selected',str(replica),project,deployment],env=env,capture_output=True,check=True,timeout=30)
    selected['NIBLEAF_SEARCH_INDEX_ROOT']=str(replica)
    log=pathlib.Path('/tmp/nibleaf-native-authority-api.log')
    with log.open('w') as handle:
        process=subprocess.Popen([str(rust/'target/debug/nibleaf-api')],env=selected,stdout=handle,stderr=handle)
        mode=sql(f'SELECT "accessMode" FROM project WHERE id=\'{project}\'')
        try:
            for _ in range(150):
                assert process.poll() is None
                try: urllib.request.urlopen(base+'/health/ready',timeout=.3).close(); break
                except (OSError,urllib.error.URLError): time.sleep(.05)
            else: raise AssertionError('native authority API startup failed')
            request('/v1/projects',401,{'Authorization':'Bearer test-owner-token'})
            request('/v1/projects',401,{'Cookie':'nibleaf_session=test-owner-token'})
            request('/v1/projects',200,native)
            session=request('/v1/identity/session',200,native)['data']
            assert isinstance(session['expires_at'],str)
            datetime.datetime.fromisoformat(session['expires_at'].replace('Z','+00:00'))
            members=request(f'/v1/projects/{project}/members',200,native)['data']
            assert members and all(isinstance(member['created_at'],str) for member in members)
            for member in members: datetime.datetime.fromisoformat(member['created_at'].replace('Z','+00:00'))
            sql(f'UPDATE project SET "accessMode"=\'WORKSPACE\' WHERE id=\'{project}\'')
            path=f'/v1/public/sites/{project}/search?q=scopeprobe'
            request(path,404)
            request(path,401,{'Authorization':'Bearer test-owner-token'})
            request(path,401,{'Cookie':'nibleaf_session=test-owner-token'})
            request(path,200,native)
            request(path,401,{**native,'Authorization':'Bearer test-owner-token'})
            manifest=replica/'projects'/hashlib.sha256(project.encode()).hexdigest()/'generations'/generation/'manifest.json'
            held=manifest.with_suffix('.held');manifest.rename(held)
            try:
                request(path,503,native)
                original=urllib.request.urlopen(urllib.request.Request('http://127.0.0.1:4316'+path,headers=native),timeout=8)
                assert original.status==200;original.read()
            finally: held.rename(manifest)
            request(path,200,native)
            sql(f"UPDATE rust_identity_session SET revoked_at=NOW(),revoke_reason='replica_revocation_proof' WHERE id='{native_session_id}'")
            try:
                request(path,401,native)
                try: urllib.request.urlopen(urllib.request.Request('http://127.0.0.1:4316'+path,headers=native),timeout=8)
                except urllib.error.HTTPError as error: assert error.code==401;error.read()
                else: raise AssertionError('first replica used stale native authority')
            finally: sql(f"UPDATE rust_identity_session SET revoked_at=NULL,revoke_reason=NULL WHERE id='{native_session_id}'")

            sql(f'UPDATE project SET "accessMode"=\'PUBLIC\' WHERE id=\'{project}\'')
            request(path,200)
            request(path,401,{'Authorization':'Bearer test-owner-token'})
        finally:
            sql(f'UPDATE project SET "accessMode"=\'{mode}\' WHERE id=\'{project}\'')
            process.terminate()
            try: process.wait(timeout=10)
            except subprocess.TimeoutExpired: process.kill(); process.wait(); raise
    temp.cleanup()
    assert 'API HTTP and search work drained' in log.read_text()
    print('native-authority-proof '+json.dumps({'http_checks':checks,'legacy_credentials_rejected':True,'native_cookie_works':True,
        'anonymous_public_retained':True,'dates_rfc3339':True,'independent_replica_roots':True,'artifact_integrity_and_corruption_isolation':True,'shared_revocation_fence':True,'production_switched':False}))
