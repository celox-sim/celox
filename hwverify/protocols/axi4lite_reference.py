"""Independent executable AXI4-Lite trace oracle, not derived from IR predicates.

Integer queues and snapshots deliberately do not evaluate the generated monitor.
Rows are pre-edge samples, including explicit rst. Reset clears an epoch.
"""

def check_trace(rows, config):
    if not isinstance(config, dict) or set(config) != {'address_width', 'data_width', 'capacity', 'role'}:
        raise ValueError('invalid AXI trace configuration')
    if config['role'] not in ('manager', 'subordinate', 'link') or any(type(config[k]) is not int or config[k] not in allowed for k, allowed in [('address_width', range(1, 65)), ('data_width', (32, 64)), ('capacity', range(1, 17))]):
        raise ValueError('unsupported AXI trace configuration')
    widths = {'awaddr': config['address_width'], 'araddr': config['address_width'], 'awprot': 3, 'arprot': 3,
              'wdata': config['data_width'], 'rdata': config['data_width'], 'wstrb': config['data_width'] // 8, 'bresp': 2, 'rresp': 2}
    payloads = {'aw': ['awaddr', 'awprot'], 'w': ['wdata', 'wstrb'], 'b': ['bresp'], 'ar': ['araddr', 'arprot'], 'r': ['rdata', 'rresp']}
    owners = {'aw': 'manager', 'w': 'manager', 'ar': 'manager', 'b': 'subordinate', 'r': 'subordinate'}
    booleans = {'rst'} | {c + suffix for c in payloads for suffix in ('valid', 'ready')}
    keys = booleans | set(widths)
    if not isinstance(rows, list) or not rows or not isinstance(rows[0], dict) or rows[0].get('rst') is not True:
        raise ValueError('trace must start with a sampled reset')
    counts = {'aw': 0, 'w': 0, 'ar': 0}; held = {}; env_invalid = False; out_of_scope = False
    addresses = []; strobes = []
    guarantees = []; environment = []; capacity = []; transfers = dict.fromkeys(payloads, 0)
    for edge, row in enumerate(rows):
        if not isinstance(row, dict) or set(row) != keys:
            raise ValueError('trace requires every AXI signal exactly once')
        if any(type(row[n]) is not bool for n in booleans) or any(type(row[n]) is not int or not 0 <= row[n] < 2**w for n, w in widths.items()):
            raise ValueError('invalid sampled signal value/type')
        violations = []
        if row['rst']:
            counts = dict.fromkeys(counts, 0); held = {}; env_invalid = False; out_of_scope = False
            addresses = []; strobes = []
            for owner in ('manager', 'subordinate'):
                if any(row[c + 'valid'] for c in payloads if owners[c] == owner):
                    violations.append((owner + '_reset_valid', owner))
        else:
            for ch, fields in payloads.items():
                if ch in held:
                    if not row[ch + 'valid']: violations.append((ch + '_valid_stable', owners[ch]))
                    if [row[n] for n in fields] != held[ch]: violations.append((ch + '_payload_stable', owners[ch]))
            if row['bvalid'] and (not counts['aw'] or not counts['w']): violations.append(('b_requires_aw_w', 'subordinate'))
            if row['rvalid'] and not counts['ar']: violations.append(('r_requires_ar', 'subordinate'))
            for ch in ('b', 'r'):
                if row[ch + 'valid'] and row[ch + 'resp'] == 1: violations.append((ch + '_response_code', 'subordinate'))
            if row['awvalid'] and row['awready']: addresses.append(row['awaddr'])
            if row['wvalid'] and row['wready']: strobes.append(row['wstrb'])
            if addresses and strobes:
                offset = addresses.pop(0) % (config['data_width'] // 8)
                strobe = strobes.pop(0)
                if any(strobe & (1 << lane) for lane in range(offset)):
                    violations.append(('write_address_strobe', 'manager'))
            # Bound storage even after an already invalid/out-of-scope prefix.
            addresses = addresses[:config['capacity']]; strobes = strobes[:config['capacity']]
        assumptions = [(n, owner) for n, owner in violations if config['role'] != 'link' and owner != config['role']]
        environment.extend({'edge': edge, 'rule': n} for n, _ in assumptions)
        if assumptions: env_invalid = True
        if not env_invalid and not out_of_scope:
            guarantees.extend({'edge': edge, 'rule': n} for n, owner in violations if config['role'] == 'link' or owner == config['role'])
        if row['rst']: continue
        for ch in payloads:
            if row[ch + 'valid'] and row[ch + 'ready']: transfers[ch] += 1
        for ch, response in [('aw', 'b'), ('w', 'b'), ('ar', 'r')]:
            pushed = int(row[ch + 'valid'] and row[ch + 'ready'])
            popped = int(row[response + 'valid'] and row[response + 'ready'])
            total = counts[ch] + pushed - popped
            if total > config['capacity']:
                capacity.append({'edge': edge, 'channel': ch}); out_of_scope = True
            counts[ch] = min(config['capacity'], max(0, total))
        held = {ch: [row[n] for n in fields] for ch, fields in payloads.items() if row[ch + 'valid'] and not row[ch + 'ready']}
    return {'guarantee_violations': guarantees, 'environment_violations': environment, 'capacity_exceeded': capacity,
            'accepted_transfers': transfers, 'outstanding': counts,
            'conditional_guarantees': 'failed' if guarantees else 'passed',
            'environment': 'invalid' if environment else 'legal_sampled_prefix',
            'capacity': 'exceeded' if capacity else 'in_scope',
            'environment_nonvacuity': {'checked': False, 'scope': 'concrete trace only; no quantified environment or cover check'},
            'status': 'protocol_violation' if guarantees else 'environment_invalid' if environment else 'scope_exceeded' if capacity else 'sampled_prefix_passed'}
