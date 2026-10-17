#!/usr/bin/env python3
"""Execute the existing app test files in fresh, sequential Vitest processes.

This controls aggregate runner memory on a 2-GiB verification host; it does not
mock application code, skip a failing file, retry assertions, or claim a build
SLO. Failures/resource kills and skipped cases remain explicit in JSON output.
"""
import argparse, json, os, pathlib, signal, subprocess, tempfile, time

ROOT=pathlib.Path(__file__).resolve().parents[1]
ACTIVE=None

def group_rss_kib(group):
    total=0
    for path in pathlib.Path('/proc').iterdir():
        if not path.name.isdigit(): continue
        try:
            fields=(path/'stat').read_text().rsplit(')',1)[1].split()
            if int(fields[2])!=group: continue
            for line in (path/'status').read_text().splitlines():
                if line.startswith('VmRSS:'): total+=int(line.split()[1]); break
        except (FileNotFoundError,ProcessLookupError):
            continue # A supervised process can exit between procfs reads.
    return total

def stop_group(process):
    try: os.killpg(process.pid,signal.SIGTERM)
    except ProcessLookupError: return
    try: process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        try: os.killpg(process.pid,signal.SIGKILL)
        except ProcessLookupError: pass
        process.wait(timeout=5)
    finally:
        # The command wrapper can exit before an orphaned worker. Do not let
        # an exited root PID hide descendants in its dedicated process group.
        try: os.killpg(process.pid,signal.SIGKILL)
        except ProcessLookupError: pass

def interrupted(_signal,_frame): raise KeyboardInterrupt

def main():
    global ACTIVE
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--evidence',type=pathlib.Path,required=True)
    parser.add_argument('--heap-mib',type=int,default=384)
    parser.add_argument('--rss-mib',type=int,default=1100)
    parser.add_argument('--file-timeout-seconds',type=int,default=150)
    parser.add_argument('--batch-size',type=int,default=1)
    args=parser.parse_args()
    if not 128<=args.heap_mib<=768 or not 512<=args.rss_mib<=1400 or not 1<=args.batch_size<=8:
        parser.error('heap 128..768 / aggregate RSS 512..1400 MiB / batch size 1..8 required')
    args.evidence.mkdir(parents=True,exist_ok=True)
    app=ROOT/'apps/app'
    files=sorted(str(path.relative_to(app)) for path in (app/'src').rglob('*') if path.is_file() and path.name.endswith(('.test.ts','.test.tsx')))
    config=(app/'vitest.config.ts').read_text()
    assert "include: ['src/**/*.test.{ts,tsx}']" in config, 'Review discovery if test config changes.'
    env={**os.environ,'NODE_OPTIONS':f'--max-old-space-size={args.heap_mib}'}
    outcomes=[]
    signal.signal(signal.SIGTERM,interrupted)
    signal.signal(signal.SIGINT,interrupted)
    try:
        for offset in range(0,len(files),args.batch_size):
            batch=files[offset:offset+args.batch_size]
            number=offset//args.batch_size
            report=args.evidence/f'vitest-{number:03}.json'; log=args.evidence/f'vitest-{number:03}.log'
            command=['pnpm','--filter','@nibleaf/app','exec','vitest','run',*batch,'--maxWorkers=1','--no-file-parallelism',
                '--pool=forks','--testTimeout=30000','--reporter=json',f'--outputFile={report}']
            start=time.monotonic(); peak=0; terminated=None
            with log.open('w') as handle:
                disk_temp=ROOT.parent/'.cache'/'nibleaf-app-tests';disk_temp.mkdir(mode=0o700,parents=True,exist_ok=True)
                temporary=tempfile.TemporaryDirectory(prefix='batch-',dir=disk_temp)
                batch_env={**env,'TMPDIR':temporary.name,'TMP':temporary.name,'TEMP':temporary.name}
                ACTIVE=subprocess.Popen(command,cwd=ROOT,env=batch_env,stdout=handle,stderr=handle,start_new_session=True)
                try:
                    while ACTIVE.poll() is None:
                        peak=max(peak,group_rss_kib(ACTIVE.pid))
                        if peak>args.rss_mib*1024: terminated='aggregate_runner_rss_limit'; break
                        if time.monotonic()-start>args.file_timeout_seconds: terminated='runner_wall_timeout'; break
                        time.sleep(.2)
                finally:
                    stop_group(ACTIVE)
                    temporary.cleanup()
                code=ACTIVE.returncode; ACTIVE=None
            data=json.loads(report.read_text()) if report.exists() else {}
            unexecuted=max(data.get('numTotalTests',0)-data.get('numPassedTests',0)-data.get('numFailedTests',0)-data.get('numPendingTests',0)-data.get('numTodoTests',0),0)
            outcome={'files':batch,'exit':code,'termination':terminated,'elapsed_seconds':round(time.monotonic()-start,3),'aggregate_peak_rss_kib':peak,
                'passed':data.get('numPassedTests',0),'failed':data.get('numFailedTests',0),'pending':data.get('numPendingTests',0),
                'todo':data.get('numTodoTests',0),'unexecuted':unexecuted,'success':code==0 and data.get('success') is True and unexecuted==0 and data.get('numPendingTests',0)==0 and data.get('numTodoTests',0)==0,'report':report.name,'log':log.name}
            outcomes.append(outcome)
            (args.evidence/'progress.json').write_text(json.dumps({'discovered_files':len(files),'completed_files':sum(len(x['files']) for x in outcomes),'results':outcomes},indent=2)+'\n')
            print(json.dumps(outcome),flush=True)
        summary={'scope':'all app files matching current Vitest include; fresh supervised process per batch; no assertion retry',
            'discovered_files':len(files),'completed_files':sum(len(x['files']) for x in outcomes),
            'passed':sum(x['passed'] for x in outcomes),'failed':sum(x['failed'] for x in outcomes),
            'pending':sum(x['pending'] for x in outcomes),'todo':sum(x['todo'] for x in outcomes),'unexecuted':sum(x['unexecuted'] for x in outcomes),
            'all_files_success':all(x['success'] for x in outcomes),'results':outcomes}
        (args.evidence/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
        print('app-bounded-summary '+json.dumps({key:value for key,value in summary.items() if key!='results'}),flush=True)
        return 0 if summary['all_files_success'] else 1
    finally:
        if ACTIVE is not None and ACTIVE.poll() is None: stop_group(ACTIVE)

if __name__=='__main__': raise SystemExit(main())
