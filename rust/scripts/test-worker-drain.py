#!/usr/bin/env python3
"""Finite disposable-DB proof that SIGTERM drains an accepted SQL delivery."""
import json, os, pathlib, signal, subprocess, sys, time, urllib.parse, urllib.request, uuid
DB=os.environ['NIBLEAF_TEST_DATABASE_URL']; parsed=urllib.parse.urlsplit(DB)
assert parsed.hostname in {'127.0.0.1','localhost'} and parsed.path=='/nibleaf_rust_test'
RUST=pathlib.Path(__file__).resolve().parents[1]
main_pid=int(sys.argv[1]); root=sys.argv[2]

def sql(statement):return subprocess.check_output(['psql',DB,'-XqAt','-v','ON_ERROR_STOP=1','-c',statement],text=True,timeout=10).strip()
def alive(pid):
    try:return pathlib.Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split()[0]!='Z'
    except FileNotFoundError:return False

def main():
    assert b'nibleaf-worker' in pathlib.Path(f'/proc/{main_pid}/cmdline').read_bytes()
    os.kill(main_pid,signal.SIGTERM)
    for _ in range(200):
        if not alive(main_pid):break
        time.sleep(.05)
    else:raise AssertionError('main worker did not drain before controlled proof')
    body=json.dumps({'name':'Worker accepted-delivery drain proof'}).encode()
    request=urllib.request.Request('http://127.0.0.1:4311/v1/projects',data=body,headers={'Authorization':'Bearer test-owner-token','Content-Type':'application/json'},method='POST')
    project=json.load(urllib.request.urlopen(request,timeout=8))['data']['id']
    uuid.UUID(project)
    event=sql(f"SELECT id FROM outbox_event WHERE event_type='project.initialized' AND aggregate_id='{project}'")
    uuid.UUID(event)
    sql(f"UPDATE outbox_event SET available_at=NOW()+interval '10 minutes' WHERE id='{event}'")
    env={**os.environ,'NIBLEAF_DATABASE_URL':DB,'NIBLEAF_AUTHORITY':'transition','NIBLEAF_WORKER_BIND_ADDR':'127.0.0.1:4323',
      'NIBLEAF_WORKER_CONCURRENCY':'1','NIBLEAF_METRICS_BEARER_TOKEN':'worker-drain-test-metrics','NIBLEAF_SEARCH_INDEX_ROOT':root,
      'NIBLEAF_INVITATION_SIGNING_KEY':'MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=',
      'NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY':'ZmVkY2JhOTg3NjU0MzIxMGZlZGNiYTk4NzY1NDMyMTA='}
    for name in ['POSTMARK_API_KEY','NIBLEAF_POSTMARK_API_KEY','SMTP_URL','NIBLEAF_SMTP_URL']:env.pop(name,None)
    log=pathlib.Path('/tmp/nibleaf-worker-drain.log');lock=None
    with log.open('w') as output:
        worker=subprocess.Popen([str(RUST/'target/debug/nibleaf-worker')],env=env,stdout=output,stderr=output)
        try:
            for _ in range(100):
                assert worker.poll() is None
                try:urllib.request.urlopen('http://127.0.0.1:4323/health/ready',timeout=.3).close();break
                except OSError:time.sleep(.05)
            else:raise AssertionError('worker not ready')
            lock=subprocess.Popen(['psql',DB,'-XqAt','-v','ON_ERROR_STOP=1','-c',"BEGIN; LOCK onboarding_run IN ACCESS EXCLUSIVE MODE; SELECT pg_sleep(4); COMMIT"],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            for _ in range(60):
                if sql("SELECT EXISTS(SELECT 1 FROM pg_locks l JOIN pg_class c ON c.oid=l.relation WHERE c.relname='onboarding_run' AND l.mode='AccessExclusiveLock' AND l.granted)")=='t':break
                time.sleep(.02)
            else:raise AssertionError('worker delivery lock absent')
            sql(f"UPDATE outbox_event SET available_at=NOW() WHERE id='{event}'")
            for _ in range(100):
                state=sql(f"SELECT state FROM outbox_event WHERE id='{event}'")
                blocked=int(sql("SELECT count(*) FROM pg_stat_activity WHERE wait_event_type='Lock' AND query LIKE '%UPDATE onboarding_run%'"))
                if state=='processing' and blocked:break
                time.sleep(.02)
            else:raise AssertionError('accepted worker SQL barrier absent')
            started=time.monotonic();worker.send_signal(signal.SIGTERM)
            try:worker.wait(timeout=.3)
            except subprocess.TimeoutExpired:pass
            else:raise AssertionError('SIGTERM dropped accepted SQL delivery')
            lock.wait(timeout=8);worker.wait(timeout=10)
            assert worker.returncode==0
            assert sql(f"SELECT state FROM outbox_event WHERE id='{event}'")=='delivered'
            assert sql(f"SELECT state FROM onboarding_run WHERE project_id='{project}'")=='complete'
        finally:
            if lock and lock.poll() is None:lock.wait(timeout=8)
            if worker.poll() is None:worker.kill();worker.wait(timeout=5)
    assert 'worker HTTP and accepted work drained' in log.read_text()
    print('worker-drain-proof '+json.dumps({'accepted_sql_drained':True,'outbox_ack_persisted':True,'process_exit':0,'seconds_after_signal':round(time.monotonic()-started,3),'production_data_changed':False}))
if __name__=='__main__':main()
