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
    p = subprocess.run([ROOT / '../target/release/lydite-replay'], input=json.dumps(request), text=True, capture_output=True, timeout=20)
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
        self.assertEqual(last['axi_write_pair_pending']['value'], independent['write_pairing']['pending'])
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
            rows = [reset, row(), a, b, row(bvalid=True), row(bvalid=True), row(bvalid=True, bready=True)]
            result = self.compare(rows, 'sampled_prefix_passed')
            self.assertEqual(result['accepted_transfers']['aw'], 1)
            self.assertEqual(result['accepted_transfers']['w'], 1)
            self.assertEqual(result['accepted_transfers']['b'], 1)
        # Continuous VALID, distinct payloads on successive accepted beats, full-capacity pop+push.
        rows = [reset, row(), row(awvalid=True, awready=True, wvalid=True, wready=True, arvalid=True, arready=True),
                *[row(awvalid=True, awready=True, awaddr=4*k, wvalid=True, wready=True, wdata=k, wstrb=k,
                      bvalid=True, bready=True, arvalid=True, arready=True, araddr=k, rvalid=True, rready=True, rdata=k) for k in range(1, 6)],
                row(bvalid=True, bready=True, rvalid=True, rready=True)]
        self.compare(rows, 'sampled_prefix_passed', config={**CONFIG, 'capacity': 1})
        # No READY fairness or arbitrary completion deadline: pending/stalled prefixes are legal.
        self.compare([reset, row(), row(arvalid=True, arready=True)] + [row(rvalid=True, rdata=42)] * 12, 'sampled_prefix_passed')
        self.compare([reset, row()] + [row(awvalid=True, awaddr=7)] * 12, 'sampled_prefix_passed')

    def test_address_strobe_offsets_zero_sparse_and_roles(self):
        for dw in (32, 64):
            lanes = dw // 8; config = {**CONFIG, 'data_width': dw}
            for offset in range(lanes):
                allowed = ((1 << lanes) - 1) ^ ((1 << offset) - 1)
                for mask in {0, allowed, 1 << offset, 1 << (lanes - 1)}:
                    self.compare([row(rst=True), row(), row(awvalid=True, awready=True, awaddr=0x40+offset, wvalid=True, wready=True, wstrb=mask)], 'sampled_prefix_passed', config=config)
                if offset:
                    rows = [row(rst=True), row(), row(wvalid=True, wready=True, wstrb=1 << (offset-1)), row(awvalid=True, awready=True, awaddr=offset)]
                    self.compare(rows, 'protocol_violation', ['write_address_strobe'], config)
            illegal = [row(rst=True), row(), row(awvalid=True, awready=True, awaddr=1, wvalid=True, wready=True, wstrb=1)]
            self.compare(illegal, 'protocol_violation', ['write_address_strobe'], {**config, 'role': 'manager'})
            self.compare(illegal, 'environment_invalid', config={**config, 'role': 'subordinate'})
        # Narrow address ports imply zero high address bits, not a wider offset.
        self.compare([row(rst=True), row(), row(awvalid=True, awready=True, awaddr=1, wvalid=True, wready=True, wstrb=2)], 'sampled_prefix_passed', config={**CONFIG, 'address_width': 1})

    def test_ordered_pairing_w_first_capacity_shift_and_reset(self):
        aw = lambda address: row(awvalid=True, awready=True, awaddr=address)
        w = lambda strobe: row(wvalid=True, wready=True, wstrb=strobe)
        for first in ('aw', 'w'):
            address = [aw(1), aw(0)]; data = [w(14), w(15)]
            self.compare([row(rst=True), row()] + (address + data if first == 'aw' else data + address), 'sampled_prefix_passed')
            bad = [w(15), w(14)]
            self.compare([row(rst=True), row()] + (address + bad if first == 'aw' else bad + address), 'protocol_violation', ['write_address_strobe'])
        self.compare([row(rst=True), row(), aw(1), row(awvalid=True, awready=True, awaddr=0, wvalid=True, wready=True, wstrb=14), w(15)], 'sampled_prefix_passed')
        # Fill every queue slot then drain, including 64-bit lane 7.
        config = {**CONFIG, 'data_width': 64, 'capacity': 16}
        addresses = [aw(n % 8) for n in range(16)]
        strobes = [w(1 << (n % 8)) for n in range(16)]
        for first, frames in [('aw', addresses + strobes), ('w', strobes + addresses)]:
            # One reset + release + 32 transfers exceeds replay's 33-frame cap.
            # Check all 16 stored slots and 15 pops natively, then the full trace
            # in the independent oracle; slot 15 must survive as the final head.
            prefix = [row(rst=True), row()] + frames[:-1]
            self.compare(prefix, 'sampled_prefix_passed', config=config)
            state = core(prefix, config)['trace'][-1]['state_after']
            self.assertEqual(int(state['axi_' + first + '_pair_0']['value']), 7 if first == 'aw' else 128)
            self.assertEqual(check_trace([row(rst=True), row()] + frames, config)['status'], 'sampled_prefix_passed')
        bad_tail = strobes[:14] + [w(1)]
        for frames in (addresses[:15] + bad_tail, bad_tail + addresses[:15]):
            self.compare([row(rst=True), row()] + frames, 'protocol_violation', ['write_address_strobe'], config)
        overflow = [row(rst=True), row(), aw(1), row(awvalid=True, awready=True, wvalid=True, wready=True, wstrb=1)]
        self.compare(overflow, 'protocol_violation', ['write_address_strobe'], {**CONFIG, 'capacity': 1})
        pending = [row(rst=True), row(), aw(1), row(rst=True), row(), aw(0), w(1)]
        self.assertEqual(check_trace(pending, CONFIG)['status'], 'sampled_prefix_passed')
        # No completed pair: do not guess a missing address or strobe.
        self.compare([row(rst=True), row(), w(15)], 'sampled_prefix_passed')

    def test_stalled_offers_checked_before_ready_and_pending_is_explicit(self):
        for dw in (32, 64):
            config = {**CONFIG, 'data_width': dw}; offset = dw // 8 - 1
            for mask in (0, 1 << offset, 1):
                aw = row(awvalid=True, awaddr=offset)
                w = row(wvalid=True, wstrb=mask)
                both = row(awvalid=True, awaddr=offset, wvalid=True, wstrb=mask)
                traces = [
                    [row(rst=True), row(), {**aw, 'awready': True}, w],
                    [row(rst=True), row(), {**w, 'wready': True}, aw],
                    [row(rst=True), row(), both, both],
                ]
                for frames in traces:
                    expected = 'protocol_violation' if mask == 1 else 'sampled_prefix_passed'
                    report = self.compare(frames, expected, ['write_address_strobe'] if mask == 1 else [], config)
                    if mask == 1:
                        self.assertEqual(report['guarantee_violations'][0]['edge'], 2 if frames[2] == both else 3)
                    else:
                        self.assertEqual(report['write_pairing']['status'], 'known_offers_checked')
            for frames in ([row(rst=True), row(), aw], [row(rst=True), row(), w]):
                report = self.compare(frames, 'sampled_prefix_passed', config=config)
                self.assertEqual(report['write_pairing']['status'], 'pending')
                self.assertTrue(report['write_pairing']['pending'])
            illegal = [row(rst=True), row(), row(awvalid=True, awaddr=offset, wvalid=True, wstrb=1)]
            self.compare(illegal, 'environment_invalid', config={**config, 'role': 'subordinate'})
            self.compare(illegal, 'protocol_violation', ['write_address_strobe'], {**config, 'role': 'manager'})

    def test_live_offers_do_not_skip_accepted_queue_positions(self):
        # Current AW is transaction 1, but live W still belongs to accepted AW0.
        frames = [row(rst=True), row(), row(awvalid=True, awready=True, awaddr=0),
                  row(awvalid=True, awaddr=1, wvalid=True, wready=True, wstrb=1)]
        report = self.compare(frames, 'sampled_prefix_passed')
        self.assertEqual(report['write_pairing']['status'], 'pending')
        report = self.compare(frames + [row(awvalid=True, awaddr=1, wvalid=True, wstrb=1)], 'protocol_violation', ['write_address_strobe'])
        self.assertEqual(report['guarantee_violations'][0]['edge'], 4)
        # Mirror the index boundary: accepted W0 is zero; live W1 is illegal.
        frames = [row(rst=True), row(), row(wvalid=True, wready=True, wstrb=0),
                  row(awvalid=True, awready=True, awaddr=1, wvalid=True, wstrb=1)]
        self.compare(frames, 'sampled_prefix_passed')
        report = self.compare(frames + [row(awvalid=True, awaddr=1, wvalid=True, wstrb=1)], 'protocol_violation', ['write_address_strobe'])
        self.assertEqual(report['guarantee_violations'][0]['edge'], 4)
        # READY may remain low forever; a stable legal known offer has no deadline.
        stable = row(awvalid=True, awaddr=1, wvalid=True, wstrb=8)
        self.compare([row(rst=True), row()] + [stable] * 20, 'sampled_prefix_passed')
        # Changing the presumed pending transaction is independently a stability fault.
        self.compare([row(rst=True), row(), stable, {**stable, 'awaddr': 0}], 'protocol_violation', ['aw_payload_stable'])

    def test_truncated_pair_queues_cannot_accuse_after_capacity_overflow(self):
        # Full unbounded manager stream is (addr,strobe): (0,1),(0,1),(1,2).
        # Capacity-1 truncation used to pair the second W with the third AW.
        frames = [row(rst=True), row(), row(awvalid=True, awready=True), row(awvalid=True, awready=True),
                  row(awvalid=True, awready=True, awaddr=1, wvalid=True, wready=True, wstrb=1),
                  row(wvalid=True, wready=True, wstrb=1), row(wvalid=True, wready=True, wstrb=2)]
        for role in ('manager', 'subordinate', 'link'):
            config = {**CONFIG, 'capacity': 1, 'role': role}
            report = self.compare(frames, 'scope_exceeded', config=config)
            self.assertFalse(report['environment_violations'])
            self.assertFalse(report['guarantee_violations'])
            self.assertEqual(report['write_pairing']['status'], 'unknown_outside_legal_scope')
            self.assertEqual(core(frames, config, 'environment')['status'], 'trace_no_failure')
            # The same stream with sufficient storage establishes legal pairing.
            self.compare(frames, 'sampled_prefix_passed', config={**config, 'capacity': 3})
            # Saturated response-accounting counters likewise lose multiplicity.
            transfer = row(awvalid=True, awready=True, wvalid=True, wready=True, arvalid=True, arready=True)
            response = row(bvalid=True, bready=True, rvalid=True, rready=True)
            counted = [row(rst=True), row(), transfer, transfer, response, response]
            bounded = self.compare(counted, 'scope_exceeded', config=config)
            self.assertFalse(bounded['environment_violations'])
            self.assertEqual(core(counted, config, 'environment')['status'], 'trace_no_failure')
            self.compare(counted, 'sampled_prefix_passed', config={**config, 'capacity': 2})
            same_edge = [row(rst=True), row(), row(awvalid=True, awready=True, awaddr=1),
                         row(awvalid=True, awready=True, wvalid=True, wready=True, wstrb=1)]
            expected = 'environment_invalid' if role == 'subordinate' else 'protocol_violation'
            required = [] if role == 'subordinate' else ['write_address_strobe']
            report = self.compare(same_edge, expected, required, config)
            self.assertEqual(report['write_pairing']['status'], 'violated')
            self.assertTrue(report['capacity_exceeded'])
            # A real earlier offered-payload fault remains recorded after overflow.
            earlier = [row(rst=True), row(), row(awvalid=True, awaddr=1, wvalid=True, wstrb=1),
                       row(awvalid=True, awready=True, awaddr=1, wvalid=True, wready=True, wstrb=1),
                       row(awvalid=True, awready=True), row(awvalid=True, awready=True)]
            full = check_trace(earlier, config)
            self.assertEqual(full['write_pairing']['status'], 'violated')
            self.assertTrue(full['capacity_exceeded'])
            observed = full['environment_violations'] if role == 'subordinate' else full['guarantee_violations']
            self.assertTrue(any(v == {'edge': 2, 'rule': 'write_address_strobe'} for v in observed))
            objective = 'guarantees' if role == 'subordinate' else 'environment'
            replayed = core(earlier, config, objective)
            state = replayed['trace'][-1]['state_after']
            self.assertTrue(state['axi_scope_bad']['value'])
            self.assertTrue(state['axi_environment_bad' if role == 'subordinate' else 'axi_bad_write_address_strobe']['value'])

    def test_first_release_pre_edge_only_and_subordinate_rules(self):
        for channel in ('aw', 'w', 'ar'):
            early = [row(rst=True), row(**{channel + 'valid': True})]
            self.compare(early, 'protocol_violation', ['manager_reset_release_valid'])
            self.compare(early, 'environment_invalid', config={**CONFIG, 'role': 'subordinate'})
            self.compare(early, 'protocol_violation', ['manager_reset_release_valid'], {**CONFIG, 'role': 'manager'})
            # The next pre-edge sample may be high, representing assertion after
            # the first released tick. No additional idle cycle is required.
            legal = [row(rst=True), row(), row(**{channel + 'valid': True})]
            report = self.compare(legal, 'sampled_prefix_passed')
            self.assertEqual(report['reset_release']['samples'][0]['edge'], 1)
            self.assertEqual(report['reset_release']['status'], 'passed')
        self.assertEqual(check_trace([row(rst=True)], CONFIG)['reset_release']['status'], 'pending')
        for channel, rule in [('b', 'b_requires_aw_w'), ('r', 'r_requires_ar')]:
            report = self.compare([row(rst=True), row(**{channel + 'valid': True})], 'protocol_violation', [rule])
            self.assertNotIn('manager_reset_release_valid', {v['rule'] for v in report['guarantee_violations']})
        repeated = [row(rst=True), row(), row(awvalid=True, awready=True), row(rst=True), row(awvalid=True)]
        self.assertIn({'edge': 4, 'rule': 'manager_reset_release_valid'}, check_trace(repeated, CONFIG)['guarantee_violations'])
        with self.assertRaises(AssertionError): core(repeated, CONFIG)

    def test_generic_first_release_guard_is_plain_native_ir(self):
        from protocols.sampled_phase import first_release_guard
        for name in ('', 's.phase', 'a b'):
            with self.assertRaises(ValueError): first_release_guard(name, True)
        doc = trace_document(CONFIG); doc['inputs']['permission'] = 'bool'
        fragment = first_release_guard('private_phase', 'i.permission')
        for key in ('state', 'reset', 'next'): doc['implementation'][key].update(fragment[key])
        doc['implementation']['next']['axi_bad_manager_reset_release_valid'] = fragment['violation']
        for first, later, expected in [(True, False, 'trace_no_failure'), (False, True, 'reset_reachable_failure')]:
            samples = [{**row(rst=True), 'permission': False}, {**row(), 'permission': first}, {**row(), 'permission': later}]
            request = {'version': 1, 'document': doc, 'mode': 'check_stimulus', 'goal': 'safety', 'inputs': samples}
            result = subprocess.run([ROOT / '../target/release/lydite-replay'], input=json.dumps(request), text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stdout)
            self.assertEqual(json.loads(result.stdout)['status'], expected)
        doc['implementation']['next']['axi_bad_manager_reset_release_valid'] = 1
        invalid = subprocess.run([ROOT / '../target/release/lydite-replay'], input=json.dumps(request), text=True, capture_output=True)
        self.assertNotEqual(invalid.returncode, 0)

    def test_every_channel_valid_and_payload_stability(self):
        fields = {'aw': ('awaddr', 'awprot'), 'w': ('wdata', 'wstrb'), 'b': ('bresp',), 'ar': ('araddr', 'arprot'), 'r': ('rdata', 'rresp')}
        for ch, payload in fields.items():
            prefix = [row(rst=True), row(), row(awvalid=True, awready=True, wvalid=True, wready=True, arvalid=True, arready=True)]
            self.compare(prefix + [row(**{ch + 'valid': True}), row()], 'protocol_violation', [ch + '_valid_stable'])
            for field in payload:
                self.compare(prefix + [row(**{ch + 'valid': True}), row(**{ch + 'valid': True, ch + 'ready': True, field: 2})],
                             'protocol_violation', [ch + '_payload_stable'])

    def test_precise_response_prerequisites_and_duplicate_accounting(self):
        for accept in [row(), row(awvalid=True, awready=True), row(wvalid=True, wready=True)]:
            self.compare([row(rst=True), row(), accept, row(bvalid=True)], 'protocol_violation', ['b_requires_aw_w'])
        self.compare([row(rst=True), row(), row(awvalid=True, awready=True, wvalid=True, wready=True, bvalid=True)], 'protocol_violation', ['b_requires_aw_w'])
        self.compare([row(rst=True), row(), row(arvalid=True, arready=True, rvalid=True)], 'protocol_violation', ['r_requires_ar'])
        self.compare([row(rst=True), row(), row(rvalid=True)], 'protocol_violation', ['r_requires_ar'])
        for ch in ('b', 'r'):
            accept = row(awvalid=True, awready=True, wvalid=True, wready=True, arvalid=True, arready=True)
            response = row(**{ch + 'valid': True, ch + 'ready': True})
            self.compare([row(rst=True), row(), accept, response, response], 'protocol_violation', ['b_requires_aw_w' if ch == 'b' else 'r_requires_ar'])
            self.compare([row(rst=True), row(), accept, row(**{ch + 'valid': True, ch + 'resp': 1})], 'protocol_violation', [ch + '_response_code'])
            for code in (0, 2, 3): self.compare([row(rst=True), row(), accept, row(**{ch + 'valid': True, ch + 'resp': code})], 'sampled_prefix_passed')

    def test_reset_roles_no_circular_assumptions_and_scope(self):
        for ch, owner in [('aw', 'manager'), ('w', 'manager'), ('ar', 'manager'), ('b', 'subordinate'), ('r', 'subordinate')]:
            self.compare([row(rst=True, **{ch + 'valid': True})], 'protocol_violation', [owner + '_reset_valid'])
        rows = [row(rst=True), row(), row(awvalid=True), row(), row(bvalid=True)]
        self.compare(rows, 'environment_invalid', config={**CONFIG, 'role': 'subordinate'})
        self.compare(rows, 'protocol_violation', ['aw_valid_stable'], config={**CONFIG, 'role': 'manager'})
        self.compare([row(rst=True), row(), row(bvalid=True)], 'protocol_violation', ['b_requires_aw_w'], config={**CONFIG, 'role': 'subordinate'})
        self.compare([row(rst=True), row(), row(bvalid=True)], 'environment_invalid', config={**CONFIG, 'role': 'manager'})
        self.compare([row(rst=True), row(), row(awvalid=True, awready=True), row(awvalid=True, awready=True)], 'scope_exceeded', config={**CONFIG, 'capacity': 1})
        # A new reset epoch discards outstanding work; response after reset is unsolicited.
        self.compare([row(rst=True), row(), row(bvalid=True)], 'protocol_violation', ['b_requires_aw_w'])
        # Native replay accepts one initial reset; multi-reset trace oracle exposes epochs explicitly.
        reference = check_trace([row(rst=True), row(), row(awvalid=True, awready=True), row(rst=True), row(bvalid=True)], CONFIG)
        self.assertEqual(reference['guarantee_violations'], [{'edge': 4, 'rule': 'b_requires_aw_w'}])

    def test_widths_capacity_and_strict_configuration(self):
        for dw in (32, 64):
            c = {**CONFIG, 'data_width': dw, 'address_width': 64, 'capacity': 16}
            self.compare([row(rst=True), row(), row(wvalid=True, wdata=2**dw-1, wstrb=2**(dw//8)-1), row(wvalid=True, wready=True, wdata=2**dw-1, wstrb=2**(dw//8)-1)], 'sampled_prefix_passed', config=c)
        for key, value in [('data_width', 16), ('capacity', 0), ('capacity', True), ('address_width', 65), ('role', 'master'), ('fairness', True)]:
            with self.assertRaises(ValueError): parameters({**CONFIG, key: value})
            with self.assertRaises(ValueError): check_trace([row(rst=True), row()], {**CONFIG, key: value})
        with self.assertRaises(ValueError): check_trace([None], CONFIG)
        with self.assertRaises(ValueError): check_trace([row(rst=True, wdata=-1)], CONFIG)
        with self.assertRaises(ValueError): check_trace([row(rst=True, awvalid=1)], CONFIG)
        with self.assertRaises(ValueError): bind({'version': 4}, CONFIG, {}, {})

    def test_exhaustive_short_write_accounting_against_integer_oracle(self):
        # Independent exhaustive traces of accepted AW/W and offered B over two edges.
        # Four state bits per edge; no stability assumptions hidden in enumeration.
        c = {**CONFIG, 'capacity': 1, 'role': 'subordinate'}
        for bits in itertools.product((False, True), repeat=6):
            samples = [row(rst=True), row()]
            for k in (0, 3): samples.append(row(awvalid=bits[k], awready=True, wvalid=bits[k+1], wready=True, bvalid=bits[k+2], bready=True))
            expected = check_trace(samples, c)
            self.compare(samples, expected['status'], config=c)

    def test_broken_generated_checker_does_not_validate_its_own_oracle(self):
        samples = [row(rst=True), row(), row(bvalid=True)]
        independent = check_trace(samples, CONFIG)
        self.assertEqual(independent['guarantee_violations'], [{'edge': 2, 'rule': 'b_requires_aw_w'}])
        broken = trace_document(CONFIG)
        broken['implementation']['next']['axi_bad_b_requires_aw_w'] = False
        request = {'version': 1, 'document': broken, 'mode': 'check_stimulus', 'goal': 'safety', 'inputs': samples}
        p = subprocess.run([ROOT / '../target/release/lydite-replay'], input=json.dumps(request), text=True, capture_output=True, check=True)
        self.assertEqual(json.loads(p.stdout)['status'], 'trace_no_failure')
        # The separate known expectation detects this deliberately broken checker.
        self.assertNotEqual(bool(independent['guarantee_violations']), json.loads(p.stdout)['status'] == 'reset_reachable_failure')

    def test_conditional_api_dispositions_and_separate_objectives(self):
        config = {**CONFIG, 'role': 'subordinate', 'capacity': 1}
        illegal = [row(rst=True), row(), row(awvalid=True), row()]
        overflow = [row(rst=True), row(), row(awvalid=True, awready=True), row(awvalid=True, awready=True)]
        for rows, failing, expected in [(illegal, 'environment', 'environment_invalid'), (overflow, 'scope', 'scope_exceeded')]:
            report = check_trace(rows, config)
            self.assertEqual(report['status'], expected)
            self.assertEqual(report['conditional_guarantees'], 'passed')
            self.assertFalse(report['environment_nonvacuity']['checked'])
            self.assertEqual(core(rows, config)['status'], 'trace_no_failure')
            self.assertEqual(core(rows, config, failing)['status'], 'reset_reachable_failure')
        self.assertEqual(check_trace(illegal, config)['environment'], 'invalid')
        self.assertEqual(check_trace(overflow, config)['capacity'], 'exceeded')
        legal = check_trace([row(rst=True), row(), row()], config)
        self.assertEqual((legal['environment'], legal['capacity']), ('legal_sampled_prefix', 'in_scope'))
        self.assertFalse(legal['environment_nonvacuity']['checked'])

    def test_fault_on_capacity_overflow_edge_is_not_masked(self):
        config = {**CONFIG, 'role': 'subordinate', 'capacity': 1}
        rows = [row(rst=True), row(), row(awvalid=True, awready=True), row(awvalid=True, awready=True, rvalid=True)]
        report = check_trace(rows, config)
        self.assertEqual(report['capacity_exceeded'], [{'edge': 3, 'channel': 'aw'}])
        self.assertEqual(report['guarantee_violations'], [{'edge': 3, 'rule': 'r_requires_ar'}])
        self.compare(rows, 'protocol_violation', ['r_requires_ar'], config)

    def test_earlier_fault_survives_later_counterpart_fault(self):
        config = {**CONFIG, 'role': 'subordinate'}
        rows = [row(rst=True), row(), row(rvalid=True, awvalid=True), row()]
        report = check_trace(rows, config)
        self.assertEqual(report['status'], 'protocol_violation')
        self.assertEqual(report['guarantee_violations'], [{'edge': 2, 'rule': 'r_requires_ar'}])
        self.assertEqual(report['environment_violations'], [{'edge': 3, 'rule': 'aw_valid_stable'}])
        self.assertEqual(core(rows, config)['status'], 'reset_reachable_failure')
        # Guarantee replay stops at the first fault. A separate scope run proceeds
        # through the later counterpart fault and exposes the sticky DUT flag.
        full = core(rows, config, 'scope')
        self.assertEqual(len(full['trace']), 4)
        state = full['trace'][-1]['state_after']
        self.assertTrue(state['axi_environment_bad']['value'])
        self.assertTrue(state['axi_bad_r_requires_ar']['value'])

    def test_same_edge_counterpart_fault_ends_conditional_prefix(self):
        config = {**CONFIG, 'role': 'subordinate'}
        rows = [row(rst=True), row(), row(awvalid=True), row(rvalid=True)]
        report = check_trace(rows, config)
        self.assertEqual(report['status'], 'environment_invalid')
        self.assertEqual(report['guarantee_violations'], [])
        self.assertEqual(report['environment_violations'], [{'edge': 3, 'rule': 'aw_valid_stable'}])
        self.assertEqual(core(rows, config)['status'], 'trace_no_failure')
        self.assertEqual(core(rows, config, 'environment')['status'], 'reset_reachable_failure')
        # With no counterpart assumptions, link mode detects both faults.
        self.compare(rows, 'protocol_violation', ['aw_valid_stable', 'r_requires_ar'], {**config, 'role': 'link'})

    def test_seeded_full_channel_sequences(self):
        rng = random.Random(20261004)
        for role in ('manager', 'subordinate', 'link'):
            for _ in range(12):
                rows = [row(rst=True), row()]
                for edge in range(5):
                    r = row()
                    for n, t in signal_types(CONFIG).items(): r[n] = bool(rng.getrandbits(1)) if t == 'bool' else rng.randrange(1 << min(t['bv'], 3))
                    rows.append(r)
                c = {**CONFIG, 'role': role}
                self.compare(rows, check_trace(rows, c)['status'], config=c)

if __name__ == '__main__': unittest.main()
