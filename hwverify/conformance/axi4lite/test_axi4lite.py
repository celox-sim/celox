"""Independent expected trace verdicts versus generated native binding replay."""
import itertools
import json
from pathlib import Path
import random
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from protocols.axi4lite import trace_document, signal_types, rules, parameters, bind
from protocols.axi4lite_reference import check_trace

CONFIG = {'address_width': 8, 'data_width': 32, 'capacity': 2, 'role': 'link'}

def row(**kw):
    result = {n: False if t == 'bool' else 0 for n, t in signal_types(CONFIG).items()}
    return {**result, 'rst': False, **kw}

def core(rows, config, objective='guarantees'):
    request = {'version': 1, 'document': trace_document(config, objective), 'mode': 'check_stimulus', 'goal': 'safety', 'inputs': rows}
    p = subprocess.run([ROOT / 'target/release/hwverify-replay'], input=json.dumps(request), text=True, capture_output=True, timeout=20)
    if p.returncode: raise AssertionError(p.stdout + p.stderr)
    return json.loads(p.stdout)

class ProtocolRules(unittest.TestCase):
    def compare(self, rows, expected, required=(), config=None):
        config = config or CONFIG
        independent = check_trace(rows, config)
        self.assertEqual(independent['status'], expected)
        self.assertTrue(set(required) <= {v['rule'] for v in independent['guarantee_violations']})
        formal = core(rows, config)
        self.assertEqual(formal['status'] == 'reset_reachable_failure', bool(independent['guarantee_violations']))
        # Concrete replay stops at its first failure; compare exactly that prefix.
        independent = check_trace(rows[:len(formal['trace'])], config)
        last = formal['trace'][-1]['state_after']
        self.assertEqual(last['axi_scope_bad']['value'], bool(independent['capacity_exceeded']))
        self.assertEqual(last['axi_environment_bad']['value'], bool(independent['environment_violations']))
        # Compare each rule, not just the overall pass/fail bit.
        for name, info in rules().items():
            if config['role'] == 'link' or info['owner'] == config['role']:
                self.assertEqual(last['axi_bad_' + name]['value'], any(v['rule'] == name for v in independent['guarantee_violations']), name)
        return independent

    def test_legal_orders_backpressure_and_continuous_transfers(self):
        reset = row(rst=True)
        for first in ('aw', 'w', 'both'):
            a = row(awvalid=first != 'w', awready=True, wvalid=first != 'aw', wready=True, wdata=0x1234, wstrb=15)
            b = row(awvalid=first == 'w', awready=True, wvalid=first == 'aw', wready=True, wdata=0x1234, wstrb=15)
            rows = [reset, a, b, row(bvalid=True), row(bvalid=True), row(bvalid=True, bready=True)]
            result = self.compare(rows, 'sampled_prefix_passed')
            self.assertEqual(result['accepted_transfers']['aw'], 1)
            self.assertEqual(result['accepted_transfers']['w'], 1)
            self.assertEqual(result['accepted_transfers']['b'], 1)
        # Continuous VALID, distinct payloads on successive accepted beats, full-capacity pop+push.
        rows = [reset, row(awvalid=True, awready=True, wvalid=True, wready=True, arvalid=True, arready=True),
                *[row(awvalid=True, awready=True, awaddr=4*k, wvalid=True, wready=True, wdata=k, wstrb=k,
                      bvalid=True, bready=True, arvalid=True, arready=True, araddr=k, rvalid=True, rready=True, rdata=k) for k in range(1, 6)],
                row(bvalid=True, bready=True, rvalid=True, rready=True)]
        self.compare(rows, 'sampled_prefix_passed', config={**CONFIG, 'capacity': 1})
        # No READY fairness or arbitrary completion deadline: pending/stalled prefixes are legal.
        self.compare([reset, row(arvalid=True, arready=True)] + [row(rvalid=True, rdata=42)] * 12, 'sampled_prefix_passed')
        self.compare([reset] + [row(awvalid=True, awaddr=7)] * 12, 'sampled_prefix_passed')

    def test_address_strobe_offsets_zero_sparse_and_roles(self):
        for dw in (32, 64):
            lanes = dw // 8; config = {**CONFIG, 'data_width': dw}
            for offset in range(lanes):
                allowed = ((1 << lanes) - 1) ^ ((1 << offset) - 1)
                for mask in {0, allowed, 1 << offset, 1 << (lanes - 1)}:
                    self.compare([row(rst=True), row(awvalid=True, awready=True, awaddr=0x40+offset, wvalid=True, wready=True, wstrb=mask)], 'sampled_prefix_passed', config=config)
                if offset:
                    rows = [row(rst=True), row(wvalid=True, wready=True, wstrb=1 << (offset-1)), row(awvalid=True, awready=True, awaddr=offset)]
                    self.compare(rows, 'protocol_violation', ['write_address_strobe'], config)
            illegal = [row(rst=True), row(awvalid=True, awready=True, awaddr=1, wvalid=True, wready=True, wstrb=1)]
            self.compare(illegal, 'protocol_violation', ['write_address_strobe'], {**config, 'role': 'manager'})
            self.compare(illegal, 'environment_invalid', config={**config, 'role': 'subordinate'})
        # Narrow address ports imply zero high address bits, not a wider offset.
        self.compare([row(rst=True), row(awvalid=True, awready=True, awaddr=1, wvalid=True, wready=True, wstrb=2)], 'sampled_prefix_passed', config={**CONFIG, 'address_width': 1})

    def test_ordered_pairing_w_first_capacity_shift_and_reset(self):
        aw = lambda address: row(awvalid=True, awready=True, awaddr=address)
        w = lambda strobe: row(wvalid=True, wready=True, wstrb=strobe)
        for first in ('aw', 'w'):
            address = [aw(1), aw(0)]; data = [w(14), w(15)]
            self.compare([row(rst=True)] + (address + data if first == 'aw' else data + address), 'sampled_prefix_passed')
            bad = [w(15), w(14)]
            self.compare([row(rst=True)] + (address + bad if first == 'aw' else bad + address), 'protocol_violation', ['write_address_strobe'])
        self.compare([row(rst=True), aw(1), row(awvalid=True, awready=True, awaddr=0, wvalid=True, wready=True, wstrb=14), w(15)], 'sampled_prefix_passed')
        # Fill every queue slot then drain, including 64-bit lane 7.
        config = {**CONFIG, 'data_width': 64, 'capacity': 16}
        addresses = [aw(n % 8) for n in range(16)]
        strobes = [w(1 << (n % 8)) for n in range(16)]
        for frames in (addresses + strobes, strobes + addresses):
            self.compare([row(rst=True)] + frames, 'sampled_prefix_passed', config=config)
        bad_tail = strobes[:-1] + [w(1)]
        for frames in (addresses + bad_tail, bad_tail + addresses):
            self.compare([row(rst=True)] + frames, 'protocol_violation', ['write_address_strobe'], config)
        overflow = [row(rst=True), aw(1), row(awvalid=True, awready=True, wvalid=True, wready=True, wstrb=1)]
        self.compare(overflow, 'protocol_violation', ['write_address_strobe'], {**CONFIG, 'capacity': 1})
        pending = [row(rst=True), aw(1), row(rst=True), aw(0), w(1)]
        self.assertEqual(check_trace(pending, CONFIG)['status'], 'sampled_prefix_passed')
        # No completed pair: do not guess a missing address or strobe.
        self.compare([row(rst=True), w(15)], 'sampled_prefix_passed')

    def test_every_channel_valid_and_payload_stability(self):
        fields = {'aw': ('awaddr', 'awprot'), 'w': ('wdata', 'wstrb'), 'b': ('bresp',), 'ar': ('araddr', 'arprot'), 'r': ('rdata', 'rresp')}
        for ch, payload in fields.items():
            prefix = [row(rst=True), row(awvalid=True, awready=True, wvalid=True, wready=True, arvalid=True, arready=True)]
            self.compare(prefix + [row(**{ch + 'valid': True}), row()], 'protocol_violation', [ch + '_valid_stable'])
            for field in payload:
                self.compare(prefix + [row(**{ch + 'valid': True}), row(**{ch + 'valid': True, ch + 'ready': True, field: 2})],
                             'protocol_violation', [ch + '_payload_stable'])

    def test_precise_response_prerequisites_and_duplicate_accounting(self):
        for accept in [row(), row(awvalid=True, awready=True), row(wvalid=True, wready=True)]:
            self.compare([row(rst=True), accept, row(bvalid=True)], 'protocol_violation', ['b_requires_aw_w'])
        self.compare([row(rst=True), row(awvalid=True, awready=True, wvalid=True, wready=True, bvalid=True)], 'protocol_violation', ['b_requires_aw_w'])
        self.compare([row(rst=True), row(arvalid=True, arready=True, rvalid=True)], 'protocol_violation', ['r_requires_ar'])
        self.compare([row(rst=True), row(rvalid=True)], 'protocol_violation', ['r_requires_ar'])
        for ch in ('b', 'r'):
            accept = row(awvalid=True, awready=True, wvalid=True, wready=True, arvalid=True, arready=True)
            response = row(**{ch + 'valid': True, ch + 'ready': True})
            self.compare([row(rst=True), accept, response, response], 'protocol_violation', ['b_requires_aw_w' if ch == 'b' else 'r_requires_ar'])
            self.compare([row(rst=True), accept, row(**{ch + 'valid': True, ch + 'resp': 1})], 'protocol_violation', [ch + '_response_code'])
            for code in (0, 2, 3): self.compare([row(rst=True), accept, row(**{ch + 'valid': True, ch + 'resp': code})], 'sampled_prefix_passed')

    def test_reset_roles_no_circular_assumptions_and_scope(self):
        for ch, owner in [('aw', 'manager'), ('w', 'manager'), ('ar', 'manager'), ('b', 'subordinate'), ('r', 'subordinate')]:
            self.compare([row(rst=True, **{ch + 'valid': True})], 'protocol_violation', [owner + '_reset_valid'])
        rows = [row(rst=True), row(awvalid=True), row(), row(bvalid=True)]
        self.compare(rows, 'environment_invalid', config={**CONFIG, 'role': 'subordinate'})
        self.compare(rows, 'protocol_violation', ['aw_valid_stable'], config={**CONFIG, 'role': 'manager'})
        self.compare([row(rst=True), row(bvalid=True)], 'protocol_violation', ['b_requires_aw_w'], config={**CONFIG, 'role': 'subordinate'})
        self.compare([row(rst=True), row(bvalid=True)], 'environment_invalid', config={**CONFIG, 'role': 'manager'})
        self.compare([row(rst=True), row(awvalid=True, awready=True), row(awvalid=True, awready=True)], 'scope_exceeded', config={**CONFIG, 'capacity': 1})
        # A new reset epoch discards outstanding work; response after reset is unsolicited.
        self.compare([row(rst=True), row(bvalid=True)], 'protocol_violation', ['b_requires_aw_w'])
        # Native replay accepts one initial reset; multi-reset trace oracle exposes epochs explicitly.
        reference = check_trace([row(rst=True), row(awvalid=True, awready=True), row(rst=True), row(bvalid=True)], CONFIG)
        self.assertEqual(reference['guarantee_violations'], [{'edge': 3, 'rule': 'b_requires_aw_w'}])

    def test_widths_capacity_and_strict_configuration(self):
        for dw in (32, 64):
            c = {**CONFIG, 'data_width': dw, 'address_width': 64, 'capacity': 16}
            self.compare([row(rst=True), row(wvalid=True, wdata=2**dw-1, wstrb=2**(dw//8)-1), row(wvalid=True, wready=True, wdata=2**dw-1, wstrb=2**(dw//8)-1)], 'sampled_prefix_passed', config=c)
        for key, value in [('data_width', 16), ('capacity', 0), ('capacity', True), ('address_width', 65), ('role', 'master'), ('fairness', True)]:
            with self.assertRaises(ValueError): parameters({**CONFIG, key: value})
            with self.assertRaises(ValueError): check_trace([row(rst=True)], {**CONFIG, key: value})
        with self.assertRaises(ValueError): check_trace([None], CONFIG)
        with self.assertRaises(ValueError): check_trace([row(rst=True, wdata=-1)], CONFIG)
        with self.assertRaises(ValueError): check_trace([row(rst=True, awvalid=1)], CONFIG)
        with self.assertRaises(ValueError): bind({'version': 4}, CONFIG, {}, {})

    def test_exhaustive_short_write_accounting_against_integer_oracle(self):
        # Independent exhaustive traces of accepted AW/W and offered B over two edges.
        # Four state bits per edge; no stability assumptions hidden in enumeration.
        c = {**CONFIG, 'capacity': 1, 'role': 'subordinate'}
        for bits in itertools.product((False, True), repeat=6):
            samples = [row(rst=True)]
            for k in (0, 3): samples.append(row(awvalid=bits[k], awready=True, wvalid=bits[k+1], wready=True, bvalid=bits[k+2], bready=True))
            expected = check_trace(samples, c)
            self.compare(samples, expected['status'], config=c)

    def test_broken_generated_checker_does_not_validate_its_own_oracle(self):
        samples = [row(rst=True), row(bvalid=True)]
        independent = check_trace(samples, CONFIG)
        self.assertEqual(independent['guarantee_violations'], [{'edge': 1, 'rule': 'b_requires_aw_w'}])
        broken = trace_document(CONFIG)
        broken['implementation']['next']['axi_bad_b_requires_aw_w'] = False
        request = {'version': 1, 'document': broken, 'mode': 'check_stimulus', 'goal': 'safety', 'inputs': samples}
        p = subprocess.run([ROOT / 'target/release/hwverify-replay'], input=json.dumps(request), text=True, capture_output=True, check=True)
        self.assertEqual(json.loads(p.stdout)['status'], 'trace_no_failure')
        # The separate known expectation detects this deliberately broken checker.
        self.assertNotEqual(bool(independent['guarantee_violations']), json.loads(p.stdout)['status'] == 'reset_reachable_failure')

    def test_conditional_api_dispositions_and_separate_objectives(self):
        config = {**CONFIG, 'role': 'subordinate', 'capacity': 1}
        illegal = [row(rst=True), row(awvalid=True), row()]
        overflow = [row(rst=True), row(awvalid=True, awready=True), row(awvalid=True, awready=True)]
        for rows, failing, expected in [(illegal, 'environment', 'environment_invalid'), (overflow, 'scope', 'scope_exceeded')]:
            report = check_trace(rows, config)
            self.assertEqual(report['status'], expected)
            self.assertEqual(report['conditional_guarantees'], 'passed')
            self.assertFalse(report['environment_nonvacuity']['checked'])
            self.assertEqual(core(rows, config)['status'], 'trace_no_failure')
            self.assertEqual(core(rows, config, failing)['status'], 'reset_reachable_failure')
        self.assertEqual(check_trace(illegal, config)['environment'], 'invalid')
        self.assertEqual(check_trace(overflow, config)['capacity'], 'exceeded')
        legal = check_trace([row(rst=True), row()], config)
        self.assertEqual((legal['environment'], legal['capacity']), ('legal_sampled_prefix', 'in_scope'))
        self.assertFalse(legal['environment_nonvacuity']['checked'])

    def test_fault_on_capacity_overflow_edge_is_not_masked(self):
        config = {**CONFIG, 'role': 'subordinate', 'capacity': 1}
        rows = [row(rst=True), row(awvalid=True, awready=True), row(awvalid=True, awready=True, rvalid=True)]
        report = check_trace(rows, config)
        self.assertEqual(report['capacity_exceeded'], [{'edge': 2, 'channel': 'aw'}])
        self.assertEqual(report['guarantee_violations'], [{'edge': 2, 'rule': 'r_requires_ar'}])
        self.compare(rows, 'protocol_violation', ['r_requires_ar'], config)

    def test_earlier_fault_survives_later_counterpart_fault(self):
        config = {**CONFIG, 'role': 'subordinate'}
        rows = [row(rst=True), row(rvalid=True, awvalid=True), row()]
        report = check_trace(rows, config)
        self.assertEqual(report['status'], 'protocol_violation')
        self.assertEqual(report['guarantee_violations'], [{'edge': 1, 'rule': 'r_requires_ar'}])
        self.assertEqual(report['environment_violations'], [{'edge': 2, 'rule': 'aw_valid_stable'}])
        self.assertEqual(core(rows, config)['status'], 'reset_reachable_failure')
        # Guarantee replay stops at the first fault. A separate scope run proceeds
        # through the later counterpart fault and exposes the sticky DUT flag.
        full = core(rows, config, 'scope')
        self.assertEqual(len(full['trace']), 3)
        state = full['trace'][-1]['state_after']
        self.assertTrue(state['axi_environment_bad']['value'])
        self.assertTrue(state['axi_bad_r_requires_ar']['value'])

    def test_same_edge_counterpart_fault_ends_conditional_prefix(self):
        config = {**CONFIG, 'role': 'subordinate'}
        rows = [row(rst=True), row(awvalid=True), row(rvalid=True)]
        report = check_trace(rows, config)
        self.assertEqual(report['status'], 'environment_invalid')
        self.assertEqual(report['guarantee_violations'], [])
        self.assertEqual(report['environment_violations'], [{'edge': 2, 'rule': 'aw_valid_stable'}])
        self.assertEqual(core(rows, config)['status'], 'trace_no_failure')
        self.assertEqual(core(rows, config, 'environment')['status'], 'reset_reachable_failure')
        # With no counterpart assumptions, link mode detects both faults.
        self.compare(rows, 'protocol_violation', ['aw_valid_stable', 'r_requires_ar'], {**config, 'role': 'link'})

    def test_seeded_full_channel_sequences(self):
        rng = random.Random(20261004)
        for role in ('manager', 'subordinate', 'link'):
            for _ in range(12):
                rows = [row(rst=True)]
                for edge in range(5):
                    r = row()
                    for n, t in signal_types(CONFIG).items(): r[n] = bool(rng.getrandbits(1)) if t == 'bool' else rng.randrange(1 << min(t['bv'], 3))
                    rows.append(r)
                c = {**CONFIG, 'role': role}
                self.compare(rows, check_trace(rows, c)['status'], config=c)

if __name__ == '__main__': unittest.main()
