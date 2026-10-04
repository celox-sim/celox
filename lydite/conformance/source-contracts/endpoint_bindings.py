"""Physical endpoint uniqueness; equal expressions alone are not duplicates."""
import re
import shutil
from protocols.axi4lite_project import replay


def run(root, good, cli, out):
    results = []
    bad = root / 'duplicate-valid-endpoint'; shutil.copytree(good, bad)
    p = bad / 'manager.veryl'; text = p.read_text()
    if len(re.findall(r'\bwvalid = 1;', text)) != 1: raise RuntimeError('VALID mutation drift')
    p.write_text(re.sub(r'\bwvalid = 1;', 'wvalid = 0;', text))
    p = bad / 'manager-project.json'; manifest = replay.load_json(p)
    manifest['signals']['bus_wvalid']['signal'] = 'awvalid'; replay.write(p, manifest)
    for obligation in ('resource', 'launch'):
        name = 'duplicate-valid-' + obligation
        result = cli(name, 'search', bad / 'write-offer.json', obligation, allowed=(2,))
        if result['status'] != 'project_error' or 'distinct physical output ports' not in result['error']:
            raise RuntimeError('duplicate physical VALID binding accepted')
        results.append({'case': name, 'status': 'passed', 'disposition': 'duplicate_physical_output_rejected'})
    # This control concerns binding identity, not full AXI protocol compliance.
    # Two actual pins may share the same normal expression; keep both represented.
    shared = root / 'distinct-valid-shared-expression'; shutil.copytree(good, shared)
    p = shared / 'manager.veryl'; header, body = p.read_text().split(') {', 1)
    body = re.sub(r'\bwvalid\b', 'wvalid_internal', body)
    p.write_text(header + ') {\n    var wvalid_internal: bit;\n    assign wvalid = awvalid;\n' + body)
    p = shared / 'manager-project.json'; manifest = replay.load_json(p)
    manifest['state']['wvalid'] = 'wvalid_internal'; replay.write(p, manifest)
    row = {n: False if n not in ('bresp', 'rresp', 'rdata') else 0 for n in manifest['inputs']}
    inputs = shared / 'start.json'; replay.write(inputs, [{**row, 'rst': True}, {**row, 'start_write': True}])
    for obligation in ('resource', 'launch'):
        name = 'shared-expression-' + obligation
        result = cli(name, 'stimulus', shared / 'write-offer.json', obligation, '--inputs', inputs)
        if result['status'] != 'trace_no_failure': raise RuntimeError(result)
        doc = replay.load_json(out / name / 'model.json'); bound = doc['implementation']['binding']['states']['WriteOffer_' + obligation]
        if bound['valid_0'] != bound['valid_1']: raise RuntimeError('equal-expression control not exercised')
        actual = replay.load_json(out / name / 'simulation.json')['trace'][1]['after']
        if actual['awvalid'] != '1' or actual['wvalid'] != '1': raise RuntimeError('distinct pin launch not simulated')
        results.append({'case': name, 'status': 'passed', 'disposition': 'distinct_physical_pins_with_equal_terms_accepted'})
    return results
