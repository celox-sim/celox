#!/usr/bin/env python3
"""Source-backed evidence for generic FIFO refinement and application offers."""
import argparse
import copy
import json
from pathlib import Path
import shutil
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from protocols.axi4lite_project import replay

CLI = ROOT / 'protocols/source_contract_project.py'
AXI = ROOT / 'protocols/axi4lite_project.py'
EXAMPLE = ROOT / 'examples/source-contracts'


def main():
    parser = argparse.ArgumentParser(description=__doc__); parser.add_argument('--out', type=Path, required=True); args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False); results = []
    def cli(name, mode, binding, obligation, *extra, allowed=(0,)):
        return replay.invoke([sys.executable, CLI, mode, binding, '--obligation', obligation, '--out', args.out / name, *extra], allowed=allowed)
    def axi(name, mode, binding, *extra):
        return replay.invoke([sys.executable, AXI, mode, binding, '--out', args.out / name, *extra])
    def passed(name, **details): results.append({'case': name, 'status': 'passed', **details})
    def expect(result, status):
        if result['status'] != status: raise RuntimeError(result)
    with tempfile.TemporaryDirectory(prefix='external source contracts ') as temp:
        root = Path(temp); good = root / 'good'; shutil.copytree(EXAMPLE, good)
        fifo = good / 'read-contract.json'; obligations = ('storage', 'capacity', 'response', 'stall')
        for obligation in obligations:
            expect(cli('read-search-' + obligation, 'search', fifo, obligation), 'bounded_no_failure')
        passed('read_all_independent_obligations', depth=10)
        bus_search = axi('read-axi-search', 'search', good / 'axi-binding.json')
        if bus_search['status'] == 'unknown':
            reason = replay.load_json(args.out / 'read-axi-search' / 'search.json').get('reason')
            if reason != 'finite solver term depth budget exhausted': raise RuntimeError(bus_search)
            results.append({'case': 'full_axi_search', 'status': 'unknown', 'depth': 10, 'reason': reason})
        else:
            expect(bus_search, 'bounded_no_failure'); passed('full_axi_search', depth=10)
        if bus_search['structural']['status'] != 'verified': raise RuntimeError(bus_search['structural'])
        for channel in ('write', 'read'):
            for obligation in ('resource', 'launch'):
                expect(cli(channel + '-offer-' + obligation, 'search', good / (channel + '-offer.json'), obligation), 'bounded_no_failure')
        passed('offer_all_independent_obligations', depth=6)
        manifest = replay.load_json(good / 'read-project.json')
        flags = {'rst', 'awvalid', 'wvalid', 'arvalid', 'bready', 'rready'}
        def row(**values): return {**{k: False if k in flags else 0 for k in manifest['inputs']}, **values}
        # Two requests enter before RVALID; stalls precede two separate completions.
        two = [row(rst=True), row(), row(arvalid=True, araddr=0), row(arvalid=True, araddr=4), row(), row(), row(), row(rready=True), row(), row(rready=True), row()]
        simultaneous = [row(rst=True), row(), row(arvalid=True, araddr=0), row(), row(), row(arvalid=True, araddr=4, rready=True), row(), row(rready=True), row()]
        full = copy.deepcopy(two)
        for i in range(4, 9): full[i].update(arvalid=True, araddr=8)
        scenarios = {'two_outstanding_stalls': two, 'simultaneous': simultaneous, 'full_backpressure': full,
                     'repeat_address': [{**r, 'araddr': 0} for r in two], 'reset_idle': [row(rst=True), row(), row()]}
        for name, frames in scenarios.items():
            stimulus = root / (name + '.json'); replay.write(stimulus, frames)
            for obligation in obligations:
                expect(cli(name + '-' + obligation, 'stimulus', fifo, obligation, '--inputs', stimulus), 'trace_no_failure')
            protocol = axi(name + '-axi', 'stimulus', good / 'axi-binding.json', '--inputs', stimulus)
            expect(protocol, 'trace_no_failure')
            if protocol['independent']['status'] != 'sampled_prefix_passed': raise RuntimeError(protocol)
            trace = replay.load_json(args.out / (name + '-storage') / 'original-replay.json')['trace']
            samples = replay.load_json(args.out / (name + '-axi') / 'axi-samples.json')
            accepted = [r['rdata'] for r in samples if r['rvalid'] and r['rready']]
            if name in ('two_outstanding_stalls', 'repeat_address'):
                if max(f['state_after']['count']['value'] for f in trace) != 2: raise RuntimeError('two-outstanding cover vacuous')
                if accepted != ([17, 34] if name == 'two_outstanding_stalls' else [17, 17]): raise RuntimeError(('incorrect response order', accepted))
            if name == 'simultaneous' and not any(r['arvalid'] and r['arready'] and r['rvalid'] and r['rready'] for r in samples): raise RuntimeError('simultaneous cover vacuous')
            if name == 'full_backpressure' and not any(r['arvalid'] and not r['arready'] for r in samples): raise RuntimeError('full cover vacuous')
            passed(name, accepted_response_data=accepted, maximum_pending=max(f['state_after']['count']['value'] for f in trace))
        # A real younger-first response and corresponding younger removal, not fake tags.
        bad = root / 'reversed'; shutil.copytree(good, bad)
        source = bad / 'ordered_read.veryl'; source.write_text(source.read_text().replace('assign select_young = 0;', "assign select_young = count == 2'd2;"))
        saved = args.out / 'reversed.regression.json'
        expect(cli('reversed-search', 'search', bad / 'read-contract.json', 'response', '--regression', saved), 'reset_reachable_failure')
        expect(cli('reversed-replay', 'replay', bad / 'read-contract.json', 'response', '--regression', saved), 'reset_reachable_failure')
        witness = replay.load_json(saved); file = root / 'reversed-witness.json'; replay.write(file, witness['inputs'])
        expect(cli('reversed-good-counterpart', 'stimulus', fifo, 'response', '--inputs', file), 'trace_no_failure')
        # The whole bus trace shows 34 then 17, with unchanged legal counts/stability.
        file = root / 'two_outstanding_stalls.json'
        result = axi('reversed-axi', 'stimulus', bad / 'axi-binding.json', '--inputs', file)
        expect(result, 'trace_no_failure')
        if result['independent']['status'] != 'sampled_prefix_passed': raise RuntimeError('reversal must preserve ordinary protocol checks')
        samples = replay.load_json(args.out / 'reversed-axi' / 'axi-samples.json')
        accepted = [r['rdata'] for r in samples if r['rvalid'] and r['rready']]
        if accepted != [34, 17]: raise RuntimeError(('not a real reversal', accepted))
        expect(cli('reversed-storage', 'stimulus', bad / 'read-contract.json', 'storage', '--inputs', file), 'reset_reachable_failure')
        passed('reversed_source_responses', accepted_response_data=accepted, ordinary_axi_checks='passed')
        for obligation in obligations:
            expect(cli('equal-reversal-' + obligation, 'stimulus', bad / 'read-contract.json', obligation, '--inputs', root / 'repeat_address.json'), 'trace_no_failure')
        passed('equal_responses_not_distinguishable', limitation='identical request/response values do not establish physical origin; no identity claim')
        changed = replay.load_json(bad / 'read-contract.json'); changed['contract']['name'] = 'RenamedQueue'; replay.write(bad / 'read-contract.json', changed)
        result = cli('stale-binding', 'replay', bad / 'read-contract.json', 'response', '--regression', saved, allowed=(2,))
        if result['status'] != 'project_error' or 'identity' not in result['error']: raise RuntimeError('stale binding accepted')
        shutil.copyfile(fifo, bad / 'read-contract.json'); source.write_text(source.read_text() + '\n// identity change\n')
        result = cli('stale-source', 'replay', bad / 'read-contract.json', 'response', '--regression', saved, allowed=(2,))
        if result['status'] != 'project_error' or 'identity' not in result['error']: raise RuntimeError('stale source accepted')
        passed('stale_source_and_contract_rejected')
        for name, old, new, obligation in [
            ('capacity_overrun', "assign arready = count != 2'd2;", 'assign arready = 1;', 'capacity'),
            ('lost_storage', 'slot0 = araddr;', "slot0 = 8'd0;", 'storage'),
            ('dropped_stall', 'if rvalid && rready { rvalid = 0; }', 'if rvalid { rvalid = 0; }', 'stall'),
            ('reset_pending', 'served_young = 0; rvalid = 0;', 'served_young = 0; rvalid = 1;', 'response'),
        ]:
            path = root / name; shutil.copytree(good, path); source = path / 'ordered_read.veryl'
            if source.read_text().count(old) != 1: raise RuntimeError('mutation drift')
            source.write_text(source.read_text().replace(old, new))
            expect(cli(name, 'search', path / 'read-contract.json', obligation), 'reset_reachable_failure'); passed(name)
        # Same-width wrong storage mapping is behaviorally rejected, not trusted as origin evidence.
        altered = replay.load_json(fifo); altered['contract']['slots'].reverse(); file = good / 'wrong-slots.json'; replay.write(file, altered)
        expect(cli('wrong-slots', 'search', file, 'storage'), 'reset_reachable_failure'); passed('wrong_storage_binding')
        for name, change in [('wrong_width', lambda c: c.update(count='slot0')),
                             ('duplicate_slot', lambda c: c.update(slots=['slot0', 'slot0'])),
                             ('fake_output', lambda c: c.update(response_data='bus_wdata')),
                             ('circular_function', lambda c: c.update(response_function='s.rdata'))]:
            altered = replay.load_json(fifo); change(altered['contract']); file = good / (name + '.json'); replay.write(file, altered)
            expect(cli(name, 'search', file, 'response', allowed=(2,)), 'project_error'); passed(name)
        # Source reset polarity and repeated-reset boundary are explicit.
        active = root / 'active-high'; shutil.copytree(good, active)
        p = active / 'ordered_read.veryl'; p.write_text(p.read_text().replace('!rst_n', 'rst_n'))
        p = active / 'read-project.json'; m = replay.load_json(p); m['reset']['active'] = 1; replay.write(p, m)
        expect(cli('active-high-reset', 'stimulus', active / 'read-contract.json', 'storage', '--inputs', root / 'two_outstanding_stalls.json'), 'trace_no_failure')
        file = root / 'repeated-reset.json'; replay.write(file, [row(rst=True), row(), row(rst=True)])
        expect(cli('repeated-reset', 'stimulus', fifo, 'storage', '--inputs', file, allowed=(2,)), 'project_error'); passed('reset_scope')
        m = replay.load_json(good / 'manager-project.json')
        def manager(**values): return {**{k: False if k not in ('bresp', 'rresp', 'rdata') else 0 for k in m['inputs']}, **values}
        for channel in ('write', 'read'):
            file = root / (channel + '-ready-low.json'); replay.write(file, [manager(rst=True), manager(**{'start_' + channel: True}), manager()])
            for obligation in ('resource', 'launch'):
                name = channel + '-ready-low-' + obligation
                expect(cli(name, 'stimulus', good / (channel + '-offer.json'), obligation, '--inputs', file), 'trace_no_failure')
            trace = replay.load_json(args.out / (channel + '-ready-low-launch') / 'original-replay.json')['trace']
            valids = ['awvalid', 'wvalid'] if channel == 'write' else ['arvalid']
            if not all(trace[1]['state_after'][v]['value'] for v in valids): raise RuntimeError('READY-low offer cover vacuous')
            mutant = root / (channel + '-ready-wait'); shutil.copytree(good, mutant); p = mutant / 'manager.veryl'
            site = 'start_' + channel + ' && !' + channel + '_busy'
            p.write_text(p.read_text().replace(site, site + (' && awready && wready' if channel == 'write' else ' && arready')))
            saved = args.out / (channel + '-wait.regression.json')
            expect(cli(channel + '-wait-search', 'search', mutant / (channel + '-offer.json'), 'launch', '--regression', saved), 'reset_reachable_failure')
            expect(cli(channel + '-wait-resource', 'search', mutant / (channel + '-offer.json'), 'resource'), 'reset_reachable_failure')
            expect(cli(channel + '-wait-replay', 'replay', mutant / (channel + '-offer.json'), 'launch', '--regression', saved), 'reset_reachable_failure')
            inputs = replay.load_json(saved)['inputs']; replay.write(file, inputs)
            expect(cli(channel + '-wait-good', 'stimulus', good / (channel + '-offer.json'), 'launch', '--inputs', file), 'trace_no_failure')
            # Concrete READY-low witness remains legal and passes structural checking.
            bus = axi(channel + '-wait-axi', 'stimulus', mutant / 'manager-binding.json', '--inputs', file)
            expect(bus, 'trace_no_failure')
            if bus['structural']['status'] != 'verified' or bus['independent']['status'] != 'sampled_prefix_passed': raise RuntimeError('registered wait witness not independently legal')
            passed(channel + '_registered_ready_wait', ordinary_axi_checks='passed', structural='verified', application_launch='violated', ready_low_cover={'edge': 1, 'valid_after': {v: trace[1]['state_after'][v]['value'] for v in valids}})
    replay.write(args.out / 'results.json', results); print(json.dumps({'status': 'passed', 'cases': len(results), 'known_unknowns': [r for r in results if r['status'] == 'unknown']}))

if __name__ == '__main__': main()
