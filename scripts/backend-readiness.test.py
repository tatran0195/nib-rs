#!/usr/bin/env python3
import copy, importlib.util, json, pathlib, tempfile, unittest
ROOT=pathlib.Path(__file__).resolve().parents[1]
def load(name,file):
    spec=importlib.util.spec_from_file_location(name,ROOT/'scripts'/file);module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module
inventory=load('backend_inventory','backend-readiness-inventory.py')
gate=load('whole_gate','check-whole-backend-readiness.py')

class BackendReadinessTests(unittest.TestCase):
    def test_native_invitation_and_guarded_public_routes_are_not_lost_to_stale_docs(self):
        result=inventory.inventory(ROOT)
        paths={route['path']:route for route in result['native_http_declarations']}
        self.assertIn('/v1/member-invitations/{token}/accept',paths)
        self.assertEqual(paths['/v1/public/sites/{project_id}/search']['gate'],'public_search_enabled')
        self.assertEqual(paths['/v1/identity/session']['gate'],'native_identity_enabled')
        self.assertFalse(result['production_ready'])
    def test_inline_legacy_handlers_and_dynamic_auth_mcp_are_explicit(self):
        result=inventory.inventory(ROOT);paths={route['path'] for route in result['legacy_http_declarations']}
        for path in ['/api/public/meta','/api/public/domains/resolve','/api/public/invitations/:id','/api/app/projects/:projectId/ai']:
            self.assertIn(path,paths)
        self.assertEqual(len(result['unresolved_dynamic_boundaries']),2)
        self.assertTrue(result['discovery_only'])
    def test_partial_and_missing_features_block_even_with_local_test_success(self):
        profile=json.loads((ROOT/'docs/production-capabilities.json').read_text());result=gate.evaluate(profile,ROOT)
        self.assertFalse(result['code_ready']);self.assertFalse(result['environment_ready']);self.assertFalse(result['production_ready'])
        self.assertGreater(len(result['code_blockers']),0)
    def test_unapproved_retirement_duplicate_ids_and_unknown_environment_cannot_pass(self):
        profile=json.loads((ROOT/'docs/production-capabilities.json').read_text());profile=copy.deepcopy(profile)
        profile['environment_available']='fake';profile['slo_approved']=True;profile['release_approval_evidence']='does-not-exist'
        profile['capabilities'].append({'id':'unrelated_billing','status':'retired','decision':'docs/production-readiness.md'})
        profile['capabilities'].append(profile['capabilities'][0])
        result=gate.evaluate(profile,ROOT)
        self.assertFalse(result['production_ready']);self.assertGreaterEqual(len(result['errors']),2)
        self.assertGreater(len(result['environment_blockers']),0)

if __name__=='__main__':unittest.main()
