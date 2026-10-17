"""Local S3 wire-contract fixture, NOT provider durability/versioning proof.
Verifies AWS4 request signatures with explicit test credentials, conditional
creates and content; supports controlled post-accept ambiguity/corruption.
"""
import datetime, hashlib, hmac, http.server, threading, time, urllib.parse, xml.sax.saxutils

ACCESS='NIBLEAFTESTACCESS'
SECRET='nibleaf-private-s3-secret-sentinel-0123456789'
BUCKET='native-artifact-test'

def digest(value):return hashlib.sha256(value).hexdigest()
def quote(value):return urllib.parse.quote(value,safe='-_.~')

class Fixture:
    def __init__(self):
        self.lock=threading.Lock();self.objects={};self.requests=[];self.accept_then_error=True
        self.hold_get=True;self.block_put=False;self.get_delay=0;self.auth_failures=0
    def snapshot(self):
        with self.lock:return list(self.requests)
    def start(self):
        self.server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);self.server.fixture=self;self.server.daemon_threads=False;self.server.block_on_close=True
        self.thread=threading.Thread(target=self.server.serve_forever);self.thread.start()
        return 'http://127.0.0.1:'+str(self.server.server_address[1])
    def close(self):
        self.server.shutdown();self.server.server_close();self.thread.join(timeout=5);assert not self.thread.is_alive()

class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'
    def log_message(self,*_):pass # Credentials/body/query are not logged by this fixture.
    def reply(self,status,body=b'',headers=None):
        self.send_response(status);self.send_header('Content-Length',str(len(body)));self.send_header('Connection','close')
        for key,value in (headers or {}).items():self.send_header(key,value)
        self.end_headers()
        if self.command!='HEAD':self.wfile.write(body)
        self.close_connection=True
    def error(self,status,code):self.reply(status,f'<Error><Code>{code}</Code><Message>fixture rejection</Message></Error>'.encode(),{'Content-Type':'application/xml'})
    def signed(self,body):
        try:
            raw=self.headers['Authorization'];algorithm,fields=raw.split(' ',1)
            parts=dict(item.strip().split('=',1) for item in fields.split(','))
            credential=parts['Credential'].split('/');signed=parts['SignedHeaders'].split(';')
            assert algorithm=='AWS4-HMAC-SHA256' and credential[0]==ACCESS and credential[2:]==['us-east-1','s3','aws4_request']
            stamp=self.headers['x-amz-date'];assert credential[1]==stamp[:8]
            parsed=urllib.parse.urlsplit(self.path)
            query='&'.join(quote(k)+'='+quote(v) for k,v in sorted(urllib.parse.parse_qsl(parsed.query,keep_blank_values=True)))
            canonical_headers=''.join(name+':'+ ' '.join(self.headers[name].strip().split())+'\n' for name in signed)
            payload=self.headers.get('x-amz-content-sha256',digest(body))
            assert payload==digest(body) or (self.command!='PUT' and payload=='UNSIGNED-PAYLOAD')
            canonical='\n'.join([self.command,urllib.parse.quote(urllib.parse.unquote(parsed.path),safe='/-_.~'),query,canonical_headers,';'.join(signed),payload])
            scope='/'.join(credential[1:]);string='AWS4-HMAC-SHA256\n'+stamp+'\n'+scope+'\n'+digest(canonical.encode())
            key=('AWS4'+SECRET).encode()
            for value in credential[1:]:key=hmac.new(key,value.encode(),hashlib.sha256).digest()
            return hmac.compare_digest(hmac.new(key,string.encode(),hashlib.sha256).hexdigest(),parts['Signature'])
        except (KeyError,ValueError,AssertionError,AttributeError):return False
    def handle_request(self):
        length=int(self.headers.get('Content-Length','0'))
        if length>64*1024*1024:return self.error(413,'EntityTooLarge')
        body=self.rfile.read(length)
        fixture=self.server.fixture
        if not self.signed(body):
            with fixture.lock:fixture.auth_failures+=1
            return self.error(403,'SignatureDoesNotMatch')
        parsed=urllib.parse.urlsplit(self.path);path=urllib.parse.unquote(parsed.path)
        if path not in {'/'+BUCKET,'/'+BUCKET+'/'} and not path.startswith('/'+BUCKET+'/'):return self.error(404,'NoSuchBucket')
        query=dict(urllib.parse.parse_qsl(parsed.query,keep_blank_values=True))
        if self.command=='GET' and query.get('list-type')=='2':
            prefix=query.get('prefix','')
            with fixture.lock:
                fixture.requests.append({'kind':'list','signed':True})
                objects=[(key,data) for key,data in fixture.objects.items() if key.startswith(prefix)]
            contents=''.join('<Contents><Key>'+xml.sax.saxutils.escape(key)+'</Key><LastModified>2026-10-16T00:00:00Z</LastModified><ETag>"fixture"</ETag><Size>'+str(len(data))+'</Size><StorageClass>STANDARD</StorageClass></Contents>' for key,data in objects)
            result=f'<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>{BUCKET}</Name><Prefix>{xml.sax.saxutils.escape(prefix)}</Prefix><KeyCount>{len(objects)}</KeyCount><MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated>{contents}</ListBucketResult>'
            return self.reply(200,result.encode(),{'Content-Type':'application/xml'})
        key=path.removeprefix('/'+BUCKET+'/')
        if self.command=='PUT':
            with fixture.lock:
                fixture.requests.append({'kind':'put','signed':True,'conditional':self.headers.get('If-None-Match')=='*','bytes':len(body)})
                if self.headers.get('If-None-Match')!='*':status=400
                elif fixture.block_put:status=503
                elif key in fixture.objects:status=412
                else:
                    fixture.objects[key]=body
                    status=500 if fixture.accept_then_error else 200
                    fixture.accept_then_error=False
                    fixture.requests.append({'kind':'accepted','signed':True,'bytes':len(body)})
            if status!=200:return self.error(status,{400:'InvalidRequest',503:'ServiceUnavailable',412:'PreconditionFailed',500:'InternalError'}[status])
            return self.reply(200,b'',{'ETag':'"'+hashlib.md5(body,usedforsecurity=False).hexdigest()+'"'})
        if self.command in {'GET','HEAD'}:
            with fixture.lock:
                fixture.requests.append({'kind':'get','signed':True})
                value=fixture.objects.get(key);held=fixture.hold_get;delay=fixture.get_delay
            if held:return self.error(503,'ServiceUnavailable')
            if value is None:return self.error(404,'NoSuchKey')
            if delay:time.sleep(delay)
            return self.reply(200,value,{'Content-Type':'application/zstd','ETag':'"'+hashlib.md5(value,usedforsecurity=False).hexdigest()+'"','Last-Modified':'Fri, 16 Oct 2026 00:00:00 GMT'})
        self.error(405,'MethodNotAllowed')
    do_GET=handle_request
    do_HEAD=handle_request
    do_PUT=handle_request
