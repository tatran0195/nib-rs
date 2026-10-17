#!/usr/bin/env python3
"""Read-only staged search migration/removal inventory. NEVER a deployment tool.

Exit 2 means release blockers remain, not an execution failure. No fallback,
artifact deletion, publication creation or automatic approval is provided.
"""
import argparse, hashlib, json, os, pathlib, re, subprocess

PATTERN = re.compile(r'@nibleaf/(?:qdrant|search)|qdrant|orama|SEARCH_[A-Z_]+|ai-search|search\.project\.(?:index|delete)', re.IGNORECASE)
RETAINED_GATES = [
    'review compatible publication inventory for every production project; republish incompatible snapshots consciously',
    'select the tested native caller adapter and update UI runtime/answer capability plus settings/diagnostics; no automatic fallback',
    'approve the selected 200-UTF16 limit versus installed Zod scalar-count behavior and language/relevance characterization',
    'validate largest production corpora, concurrent traffic, replica/storage/network failure and process-manager resource/termination policy',
    'rehearse PostgreSQL plus real index-volume backup recovery and retained-store writer fence/GC/erasure',
    'remove both legacy engines/jobs/providers/flags and update manifests/lock/docs only in the reviewed compatibility release',
]

def source_inventory(root):
    paths = []
    for directory in ['apps','packages']:
        for base, dirs, files in os.walk(root/directory):
            dirs[:] = [d for d in dirs if d not in {'node_modules','dist','build','.output','.cache','generated','paraglide','.turbo'}]
            for name in files:
                path=pathlib.Path(base)/name
                if path.suffix in {'.ts','.tsx','.js','.mjs','.json','.yaml','.md','.mdx'}:
                    paths.append(path)
    paths.extend(root/name for name in ['package.json','pnpm-lock.yaml','.env.example','.env.production.example'])
    found={}
    for path in sorted(set(paths)):
        content=path.read_text(encoding='utf-8')
        matches=sorted(set(PATTERN.findall(content)))
        if matches:
            found[str(path.relative_to(root))]={'sha256':hashlib.sha256(path.read_bytes()).hexdigest(),'references':matches}
    return found

def publication_inventory(database, maximum):
    statement=f'''BEGIN READ ONLY; SET LOCAL statement_timeout='5s';
      WITH latest AS (
        SELECT DISTINCT ON ("projectId") id,"projectId",version,snapshot FROM deployment
        WHERE status='READY' ORDER BY "projectId",version DESC,id DESC
      ), inventory AS (
        SELECT project.id AS project_id, latest.id AS deployment_id,latest.version,
          latest.snapshot->>'schema_version' AS snapshot_format,generation.generation_id,
          generation.state,generation.document_count,generation.documents_sha256,generation.delivery_kind,
          artifact.artifact_sha256,artifact.artifact_bytes,artifact.object_key
        FROM project LEFT JOIN latest ON latest."projectId"=project.id
        LEFT JOIN rust_search_generation AS generation ON generation.project_id=project.id
          AND generation.tenant_id=project."organizationId" AND generation.deployment_id=latest.id
        LEFT JOIN rust_search_artifact AS artifact ON artifact.generation_id=generation.generation_id
          AND artifact.project_id=generation.project_id AND artifact.tenant_id=generation.tenant_id
        ORDER BY project.id LIMIT {maximum+1}
      ) SELECT COALESCE(json_agg(inventory),'[]') FROM inventory; COMMIT;'''
    output=subprocess.check_output(['psql',database,'-XqAt','-v','ON_ERROR_STOP=1','-c',statement],text=True,timeout=15)
    return json.loads(output)

def plan(root, rows=None, verified=None, overflow=False):
    inventory=source_inventory(root)
    blocked=[]
    if rows is None: blocked.append('No deployment database inventory supplied.')
    if overflow: blocked.append('Publication inventory overflow; never truncate into approval.')
    for row in rows or []:
        if row['snapshot_format']!='2' or row['state'] not in {'ready','superseded'} or row['generation_id'] is None or (row.get('delivery_kind')=='object' and not row.get('artifact_sha256')):
            blocked.append('Publication unavailable/incompatible for project '+row['project_id'])
    if not verified: blocked.append('Native selected snapshot/registry/index evidence has not all been verified.')
    if inventory: blocked.append('Legacy runtime references remain; this candidate does not remove production authority.')
    blocked.extend(RETAINED_GATES)
    return {'kind':'staged_search_cutover_preflight','deploy':False,'delete_data':False,'automatic_fallback':False,
        'ready_for_production':False,'publication_inventory':rows,'inventory_overflow':overflow,
        'verified_projects':verified or [],'legacy_source_inventory':inventory,'blockers':blocked,
        'removal_order':[
            {'phase':1,'action':'review per-project source-format-2 / manifest-3 serving evidence and publication migration'},
            {'phase':2,'action':'explicitly select native client factory; preserve forwarding/no-store/abort and disable answer UI'},
            {'phase':3,'action':'reconcile project config, integration cards, diagnostics, analytics and public schemas'},
            {'phase':4,'action':'remove Orama cache/package plus Qdrant adapter/provider/search jobs and legacy flags; no retained shadow/fallback'},
            {'phase':5,'action':'regenerate Node lock, run both runtime suites and migration/recovery/erasure gates; release only after external approval'},
        ]}

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root',type=pathlib.Path,default=pathlib.Path(__file__).resolve().parents[1])
    parser.add_argument('--database-env',default='NIBLEAF_DATABASE_URL')
    parser.add_argument('--index-root',type=pathlib.Path)
    parser.add_argument('--max-projects',type=int,default=1000)
    parser.add_argument('--output',type=pathlib.Path)
    args=parser.parse_args()
    if not 1<=args.max_projects<=10000: parser.error('max-projects must be 1..10000')
    database=os.environ.get(args.database_env)
    rows=publication_inventory(database,args.max_projects) if database else None
    overflow=rows is not None and len(rows)>args.max_projects
    verified=[]
    if rows and not overflow and args.index_root:
        for row in rows:
            if row['snapshot_format']!='2' or row['state'] not in {'ready','superseded'}: continue
            if row.get('delivery_kind')=='object': continue # Local-root verification cannot qualify remote artifact/runtime state.
            child=subprocess.run([str(args.root/'rust/target/debug/search-ops'),'verify-selected',str(args.index_root),row['project_id'],row['deployment_id']],
                env={**os.environ,'NIBLEAF_DATABASE_URL':database},capture_output=True,timeout=40)
            if child.returncode==0:
                receipt=json.loads(child.stdout)
                if receipt['documents_sha256']==row['documents_sha256'] and receipt['documents']==row['document_count']:
                    verified.append(row['project_id'])
    result=plan(args.root,rows,verified,overflow)
    if rows and len(verified)!=len(rows): result['blockers'].insert(0,'Not every inventoried project has verified native artifacts.')
    text=json.dumps(result,indent=2)+'\n'
    if args.output: args.output.write_text(text)
    else: print(text,end='')
    return 2 # Production approval cannot be manufactured by a local source scan.

if __name__=='__main__': raise SystemExit(main())
