#!/usr/bin/env python3
"""Local TCP-blackhole drill: distinguish response deadline, drain and SIGKILL.
No claim that the application can hard-preempt a failed network is made.
"""
import concurrent.futures, contextlib, json, os, pathlib, signal, socket, socketserver, subprocess, threading, time, urllib.parse, urllib.request, urllib.error

class Proxy(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = False
    block_on_close = True

class Relay(socketserver.BaseRequestHandler):
    def handle(self):
        backend = socket.create_connection(('127.0.0.1',5432),timeout=2)
        stopped = threading.Event()
        def copy(source,target):
            source.settimeout(.1); target.settimeout(.1)
            try:
                while not self.server.stop.is_set() and not stopped.is_set():
                    if not self.server.gate.wait(.1): continue
                    try: data = source.recv(65536)
                    except socket.timeout: continue
                    if not data: break
                    while not self.server.gate.wait(.1):
                        if self.server.stop.is_set() or stopped.is_set(): return
                    target.sendall(data)
            except OSError: pass # Explicit fault-injected socket shutdown only.
            finally: stopped.set()
        threads = [threading.Thread(target=copy,args=pair) for pair in [(self.request,backend),(backend,self.request)]]
        for thread in threads: thread.start()
        for thread in threads: thread.join()
        backend.close()

def run(env, rust, project, sql):
    server = Proxy(('127.0.0.1',0),Relay)
    server.stop = threading.Event(); server.gate = threading.Event(); server.gate.set()
    runner = threading.Thread(target=server.serve_forever); runner.start()
    db = urllib.parse.urlsplit(env['NIBLEAF_DATABASE_URL'])
    assert db.hostname == '127.0.0.1' and db.path == '/nibleaf_rust_test'
    netloc = db.netloc.rsplit('@',1)[0]+'@127.0.0.1:'+str(server.server_address[1])
    child_env = {**env,'NIBLEAF_DATABASE_URL':urllib.parse.urlunsplit(db._replace(netloc=netloc)), 'NIBLEAF_BIND_ADDR':'127.0.0.1:4318'}
    observations=[]
    try:
        for forced in [False,True]:
            path=pathlib.Path('/tmp/nibleaf-search-network-'+str(forced)+'.log')
            with path.open('w') as log:
                process=subprocess.Popen([str(rust/'target/debug/nibleaf-api')],env=child_env,stdout=log,stderr=log)
                lock=None
                try:
                    for _ in range(150):
                        assert process.poll() is None
                        try: urllib.request.urlopen('http://127.0.0.1:4318/health/ready',timeout=.2).close(); break
                        except (OSError,urllib.error.URLError): time.sleep(.05)
                    else: raise AssertionError('network API startup failed')
                    lock=subprocess.Popen(['psql',env['NIBLEAF_DATABASE_URL'],'-XqAt','-c',"BEGIN; LOCK rust_search_generation IN ACCESS EXCLUSIVE MODE; SELECT pg_sleep(3); COMMIT"],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
                    for _ in range(50):
                        if sql("SELECT EXISTS(SELECT 1 FROM pg_locks l JOIN pg_class c ON c.oid=l.relation WHERE c.relname='rust_search_generation' AND l.mode='AccessExclusiveLock' AND l.granted)")=='t': break
                        time.sleep(.02)
                    def query():
                        try: return urllib.request.urlopen(f'http://127.0.0.1:4318/v1/public/sites/{project}/search?q=scopeprobe',timeout=6).status
                        except urllib.error.HTTPError as error: error.read(); return error.code
                    with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                        pending=pool.submit(query)
                        for _ in range(60):
                            if int(sql("SELECT count(*) FROM pg_stat_activity WHERE wait_event_type='Lock' AND query LIKE '%nibleaf:published-search-selection%'"))>=1: break
                            time.sleep(.01)
                        else: raise AssertionError('network SQL barrier absent')
                        server.gate.clear() # Hold actual in-flight SQL completion, not pool acquisition.
                        assert pending.result(timeout=6)==503
                        process.send_signal(signal.SIGTERM)
                        try: process.wait(timeout=.3)
                        except subprocess.TimeoutExpired: pass
                        else: raise AssertionError('stalled SQL was not retained through shutdown')
                        if forced:
                            process.kill(); process.wait(timeout=2)
                            assert process.returncode == -signal.SIGKILL
                        else:
                            server.gate.set(); process.wait(timeout=8)
                            assert process.returncode==0
                    observations.append({'force_kill':forced,'response_deadline_503':True,'drain_waited_for_network':True,'application_drain':not forced})
                finally:
                    server.gate.set()
                    if process.poll() is None: process.kill(); process.wait(timeout=3)
                    if lock: lock.wait(timeout=8)
            assert ('API HTTP and search work drained' in path.read_text()) != forced
    finally:
        server.stop.set(); server.gate.set(); server.shutdown(); server.server_close(); runner.join(timeout=3)
        assert not runner.is_alive()
    print('network-drain-proof '+json.dumps(observations))
