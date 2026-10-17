#!/usr/bin/env python3
"""Read-only route/worker/source discovery for WHOLE-backend readiness.

This is source evidence, not parity/authorization/test or deployment approval.
Dynamic routes and feature completeness require explicit review. No counts
are used as a substitute for implemented behavior or executable gate evidence.
"""
import argparse, hashlib, json, pathlib, re
ROOT=pathlib.Path(__file__).resolve().parents[1]
VERBS={'get','post','put','patch','delete','head','options','all'}

def digest(path): return hashlib.sha256(path.read_bytes()).hexdigest()
def join(prefix, path): return '/'+('/'.join([prefix.strip('/'),path.strip('/')])).strip('/')
def module_path(path):
    for candidate in [path.with_suffix('.ts'),path/'index.ts']:
        if candidate.is_file(): return candidate
    return None

def balanced(text,start):
    depth=0; quote=None; escape=False
    for at in range(start,len(text)):
        c=text[at]
        if quote:
            if escape: escape=False
            elif c=='\\': escape=True
            elif c==quote: quote=None
        elif c in ('"',"'",'`'): quote=c
        elif c=='(': depth+=1
        elif c==')':
            depth-=1
            if depth==0: return at
    raise ValueError('unbalanced route call')

def rust_routes(root):
    result=[]; sources={}
    for path in sorted((root/'rust').glob('**/src/*.rs')):
        if 'vendor' in path.parts: continue
        text=path.read_text();prod=text.split('#[cfg(test)]\nmod tests',1)[0]
        if 'fn router(' not in prod and 'fn public_router(' not in prod: continue
        for match in re.finditer(r'\.route\s*\(\s*"([^"\n]+)"\s*,',prod):
            end=balanced(prod,prod.index('(',match.start()))
            expression=prod[match.end():end]
            methods=sorted(set(re.findall(r'\b(get|post|put|patch|delete|head|options)\s*\(',expression)))
            relative=str(path.relative_to(root))
            prefix='' if relative=='rust/apps/api/src/main.rs' else '/v1'
            gate='native_identity_enabled' if '/identity/' in relative else 'public_search_enabled' if path.name=='public_api.rs' else None
            result.append({'path':join(prefix,match.group(1)),'methods':methods,'source':relative,'line':prod.count('\n',0,match.start())+1,'gate':gate,
                'method_inference_complete':bool(methods)})
            sources[relative]=digest(path)
    return result,sources

def legacy_routes(root):
    result=[];sources={};unresolved=[];seen=set()
    def walk(path,prefix):
        if (path,prefix) in seen: return
        seen.add((path,prefix));text=path.read_text();relative=str(path.relative_to(root));sources[relative]=digest(path)
        imports={name:module_path(path.parent/location) for name,location in re.findall(r"import\s+(\w+)\s+from\s+['\"](\.[^'\"]+)['\"]",text)}
        for match in re.finditer(r"\.route\s*\(\s*['\"]([^'\"]*)['\"]\s*,\s*(\w+)\s*\)",text):
            target=imports.get(match.group(2))
            if target is None: unresolved.append({'source':relative,'mount':match.group(1),'symbol':match.group(2)})
            else: walk(target,join(prefix,match.group(1)))
        # Handler files use fluent line-start Hono route declarations. This
        # deliberately excludes ctx.headers.get/map.get/business method calls.
        for match in re.finditer(r"(?:^|\n)\s*(?:const\s+\w+\s*=\s*new\s+Hono(?:<[^>]+>)?\(\))?\.(get|post|put|patch|delete|head|options|all)\s*\(\s*['\"]([^'\"]*)['\"]\s*,",text):
            result.append({'path':join(prefix,match.group(2)),'methods':[match.group(1)],'source':relative,'line':text.count('\n',0,match.start())+1})
    walk(root/'apps/server/src/modules/index.ts','/api')
    # Authentication/MCP are composed outside the modules root and must never
    # disappear merely because the ordinary handler inventory is complete.
    for name in ['apps/server/src/routes.ts','apps/server/src/modules/mcp/index.ts']:
        p=root/name
        if p.is_file(): sources[name]=digest(p)
    unresolved.extend([{'source':'apps/server/src/routes.ts','mount':'/api/auth/*','symbol':'Better Auth dynamic handler'},
        {'source':'apps/server/src/routes.ts','mount':'/api/mcp','symbol':'MCP handler/session protocol, review separately'}])
    return result,sources,unresolved

def inventory(root):
    native,native_sources=rust_routes(root);legacy,legacy_sources,unresolved=legacy_routes(root)
    worker_files=sorted([*root.glob('apps/worker/src/processors/**/*.ts'),*root.glob('rust/apps/worker/src/*.rs')])
    workers={str(path.relative_to(root)):digest(path) for path in worker_files if path.is_file()}
    migrations={str(path.relative_to(root)):digest(path) for path in sorted(root.glob('rust/migrations/*.sql'))}
    return {'scope':'whole backend, multi-host, 5000-document initial benchmark, local-only qualification',
        'discovery_only':True,'production_ready':False,
        'native_http_declarations':native,'legacy_http_declarations':legacy,'unresolved_dynamic_boundaries':unresolved,
        'native_source_sha256':native_sources,'legacy_source_sha256':legacy_sources,'worker_source_sha256':workers,'rust_migrations_sha256':migrations,
        'warnings':['Source declarations are not route parity or tests. Guarded routes may be disabled.',
            'Client contracts, provider effects, authorization, retries, schedules, migrations/rollback and retirement need individual evidence.',
            'Current shared-filesystem search implementation does not establish multi-host consistency, distribution, failure fencing or offline-replica erasure.',
            'Only Google sign-in at native cutover and dense/answer search/raw-term collection have explicit selected retirements; do not silently retire unrelated AI/Git/analytics/billing features.']}

def main():
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--root',type=pathlib.Path,default=ROOT);parser.add_argument('--output',type=pathlib.Path,required=True)
    args=parser.parse_args();result=inventory(args.root);args.output.write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps({'native_route_declarations':len(result['native_http_declarations']),'legacy_route_declarations':len(result['legacy_http_declarations']),
        'legacy_handler_sources':len(result['legacy_source_sha256']),'worker_sources':len(result['worker_source_sha256']),
        'dynamic_boundaries':len(result['unresolved_dynamic_boundaries']),'production_ready':False}))
if __name__=='__main__': main()
