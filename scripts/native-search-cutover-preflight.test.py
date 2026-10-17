#!/usr/bin/env python3
import importlib.util
import pathlib
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name('native-search-cutover-preflight.py')
spec = importlib.util.spec_from_file_location('preflight', SCRIPT)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

class PreflightTests(unittest.TestCase):
    def test_legacy_inventory_is_hashed_and_never_an_automatic_deletion(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name, content in {'apps/server/search.ts': "import x from '@orama/orama';", 'packages/qdrant/package.json': '{"name":"@nibleaf/qdrant"}',
                'package.json':'{}','pnpm-lock.yaml':'{}','.env.example':'PLACEHOLDER=true','.env.production.example':'PLACEHOLDER=true'}.items():
                path=root/name; path.parent.mkdir(parents=True,exist_ok=True); path.write_text(content)
            result=module.plan(root)
            self.assertFalse(result['delete_data'])
            self.assertFalse(result['ready_for_production'])
            self.assertIn('apps/server/search.ts',result['legacy_source_inventory'])
            digest=result['legacy_source_inventory']['apps/server/search.ts']['sha256']
            (root/'apps/server/search.ts').write_text("import changed from '@orama/orama';")
            self.assertNotEqual(digest,module.source_inventory(root)['apps/server/search.ts']['sha256'])
    def test_local_verification_cannot_manufacture_production_approval(self):
        with tempfile.TemporaryDirectory() as directory:
            root=pathlib.Path(directory)
            for name in ['package.json','pnpm-lock.yaml','.env.example','.env.production.example']:
                (root/name).write_text('{}')
            result=module.plan(root, [{'project_id':'project','snapshot_format':'2','state':'ready','generation_id':'uuid'}], ['project'])
            self.assertFalse(result['ready_for_production'])
            self.assertFalse(result['automatic_fallback'])
            self.assertGreater(len(result['blockers']),0)
    def test_overflow_and_unmigrated_projects_are_blockers(self):
        with tempfile.TemporaryDirectory() as directory:
            root=pathlib.Path(directory)
            for name in ['package.json','pnpm-lock.yaml','.env.example','.env.production.example']:
                (root/name).write_text('{}')
            result=module.plan(root,[{'project_id':'legacy','snapshot_format':None,'state':None,'generation_id':None}],[],True)
            self.assertTrue(result['inventory_overflow'])
            self.assertTrue(any('legacy' in reason for reason in result['blockers']))
            self.assertTrue(any('overflow' in reason for reason in result['blockers']))

if __name__=='__main__': unittest.main()
