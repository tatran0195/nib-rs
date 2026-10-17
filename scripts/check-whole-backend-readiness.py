#!/usr/bin/env python3
"""Evaluate declared whole-backend release gates; read-only, never deploys.

Partial/missing capabilities, unapproved retirements, unmapped legacy contracts,
unapproved SLOs and local-only qualification all block independently. Source
route counts are discovery, not parity. Local tests cannot certify providers.
"""
import argparse, json, pathlib
ROOT=pathlib.Path(__file__).resolve().parents[1]
APPROVED_RETIREMENTS={'google_sign_in','dense_answer_search','raw_search_term_analytics'}
STATUSES={'complete','partial','missing','retired'}

def evaluate(profile,root):
    errors=[];code=[];environment=[];ids=set()
    if profile.get('schema_version')!=1 or profile.get('target')!='whole_backend_multi_host':errors.append('Unrecognized release profile schema/target.')
    capabilities=profile.get('capabilities',[])
    if not capabilities:errors.append('No capabilities declared; an empty inventory cannot pass.')
    for item in capabilities:
        identifier=item.get('id');status=item.get('status')
        if not identifier or identifier in ids:errors.append('Missing/duplicate capability ID.');continue
        ids.add(identifier)
        if status not in STATUSES:errors.append('Invalid capability status: '+identifier);continue
        if status=='retired':
            if identifier not in APPROVED_RETIREMENTS:errors.append('Retirement was not selected by the user: '+identifier)
            decision=item.get('decision')
            if not decision or not (root/decision).is_file():errors.append('Missing retirement decision: '+identifier)
        elif status!='complete':code.append({'capability':identifier,'status':status,'required':item.get('remaining','Complete reviewed implementation, contracts, migration and proof.')})
        else:
            receipts=item.get('evidence',[])
            if not receipts or any(not (root/path).is_file() for path in receipts):code.append({'capability':identifier,'status':'unverified_complete_claim','required':'Executed current-source evidence and full consumer/migration coverage.'})
    if not profile.get('legacy_contract_mapping_verified',False):code.append({'capability':'legacy_contract_mapping','status':'unverified','required':'Review each original route, worker/provider effect, client and migration; a route count is not parity.'})
    if not profile.get('slo_approved',False):environment.append('Measured capacity/SLOs have not been approved for the selected supported workload/topology.')
    if profile.get('environment_available')!='qualified_isolated_staging':environment.append('No recognized qualified multi-host/provider environment; local or unknown scope is not production qualification.')
    approval=profile.get('release_approval_evidence')
    if not isinstance(approval,str) or not (root/approval).is_file():
        environment.append('No production release approval/qualified deployment evidence file supplied.')
    else:
        receipt=json.loads((root/approval).read_text())
        required={'multi_host_failure','provider_retention','full_backup_restore','migration_rollback','capacity_slo','source_review'}
        if receipt.get('kind')!='reviewed_multi_host_release_approval' or receipt.get('approved') is not True or not receipt.get('reviewer'):
            environment.append('Unrecognized/missing authorized release approval.')
        if set(receipt.get('qualified_checks',{}))!=required or any(value!='executed_pass' for value in receipt.get('qualified_checks',{}).values()):
            environment.append('Environment approval does not cover all required executed qualification checks.')
        evidence=receipt.get('evidence_files',[])
        if not evidence or any(not (root/path).is_file() for path in evidence):environment.append('Qualified approval evidence files missing.')
    return {'kind':'whole_backend_readiness','code_ready':not errors and not code,'environment_ready':not environment,
        'production_ready':not errors and not code and not environment,'errors':errors,'code_blockers':code,'environment_blockers':environment,
        'scope':'whole Rust backend; no auto-deploy, production mutation or provider delete','declared_capabilities':len(capabilities)}

def main():
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--profile',type=pathlib.Path,default=ROOT/'docs/production-capabilities.json');parser.add_argument('--output',type=pathlib.Path)
    args=parser.parse_args();result=evaluate(json.loads(args.profile.read_text()),ROOT);text=json.dumps(result,indent=2)+'\n'
    if args.output:args.output.write_text(text)
    else:print(text,end='')
    return 0 if result['production_ready'] else 2
if __name__=='__main__':raise SystemExit(main())
