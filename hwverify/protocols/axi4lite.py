"""Parameterized sampled AXI4-Lite contracts; Arm IHI 0022H A3.1–A3.4/B1.1.

Pure canonical-IR generation. No DUT guarantee is an environment assumption.
Counters are observation-only ghosts, never simulation inputs or DUT state.
"""
import copy

SOURCE = 'https://developer.arm.com/-/media/Arm%20Developer%20Community/PDF/IHI0022H_amba_axi_protocol_spec.pdf'
CHANNELS = {'aw': ('manager', ('awaddr', 'awprot')), 'w': ('manager', ('wdata', 'wstrb')),
            'b': ('subordinate', ('bresp',)), 'ar': ('manager', ('araddr', 'arprot')),
            'r': ('subordinate', ('rdata', 'rresp'))}
UNCHECKED = ['input-to-output combinational paths', 'VALID independence from READY and cross-channel causality',
             'asynchronous reset assertion, deassertion and reset-release VALID timing', 'functional address/data/strobe semantics and response ordering by transaction identity',
             'eventual completion, fairness, throughput and response deadlines', 'X/Z, CDC, bursts, IDs, AXI4/AXI5 extensions']

def parameters(config):
    if not isinstance(config, dict) or set(config) != {'address_width', 'data_width', 'capacity', 'role'}:
        raise ValueError('AXI config requires address_width, data_width, capacity, role')
    for key, allowed in [('address_width', range(1, 65)), ('data_width', (32, 64)), ('capacity', range(1, 17))]:
        if type(config[key]) is not int or config[key] not in allowed:
            raise ValueError(f'unsupported AXI {key}')
    if config['role'] not in ('manager', 'subordinate', 'link'):
        raise ValueError('role must be manager, subordinate or link')
    return config

def signal_types(config):
    parameters(config)
    result = {c + suffix: 'bool' for c in CHANNELS for suffix in ('valid', 'ready')}
    for name, width in [('awaddr', config['address_width']), ('araddr', config['address_width']),
                        ('awprot', 3), ('arprot', 3), ('wdata', config['data_width']), ('rdata', config['data_width']),
                        ('wstrb', config['data_width'] // 8), ('bresp', 2), ('rresp', 2)]:
        result[name] = {'bv': width}
    return result

def rules():
    result = {}
    for ch, (owner, _) in CHANNELS.items():
        for suffix in ('valid', 'payload'):
            result[ch + '_' + suffix + '_stable'] = {'owner': owner, 'section': 'A3.2.1–A3.2.2'}
    for name in ('b_requires_aw_w', 'r_requires_ar'):
        result[name] = {'owner': 'subordinate', 'section': 'A3.3–A3.3.1'}
    for name in ('b_response_code', 'r_response_code'):
        result[name] = {'owner': 'subordinate', 'section': 'B1.1.1'}
    for owner in ('manager', 'subordinate'):
        result[owner + '_reset_valid'] = {'owner': owner, 'section': 'A3.1.2'}
    result['write_address_strobe'] = {'owner': 'manager', 'section': 'A3.4.4/B1.1.3'}
    return result

def expr(op, *args): return [op, *args]
def all_of(*args):
    result = True
    for value in args: result = expr('and', result, value)
    return result

def any_of(*args):
    result = False
    for value in args: result = expr('or', result, value)
    return result

def inv(x): return expr('not', x)
def eq(x, y): return expr('eq', x, y)
def ite(c, t, f): return expr('ite', c, t, f)
def bv(w, n): return ['bv', w, n]
def substitute(value, mapping):
    if isinstance(value, str): return copy.deepcopy(mapping.get(value, value))
    if isinstance(value, list): return [substitute(v, mapping) for v in value]
    return value


def monitor(config, signals, reset_signals):
    """One edge consumes pre-edge bus samples. Reset uses reset-established samples.

    Capacity is a tool bound, not an AXI rule. Exceeding it permanently invalidates
    the epoch. Counterpart violations likewise end the conditional proof prefix.
    Flags retain earlier DUT failures; neither event can erase a failure.
    """
    types = signal_types(config)
    if set(signals) != set(types) or set(reset_signals) != set(types):
        raise ValueError('map exactly the AXI4-Lite signal set')
    cap = config['capacity']; width = (cap + 1).bit_length()
    state = {}; reset = {}; nxt = {}; violations = {}
    def add(name, ty, initial, following):
        state[name] = ty; reset[name] = initial; nxt[name] = following
    def s(name): return 's.axi_' + name
    def reg(name, ty, initial, following): add('axi_' + name, ty, initial, following)
    for ch, (_, payload) in CHANNELS.items():
        valid, ready = signals[ch + 'valid'], signals[ch + 'ready']
        violations[ch + '_valid_stable'] = all_of(s(ch + '_held'), inv(valid))
        violations[ch + '_payload_stable'] = all_of(s(ch + '_held'), any_of(*(inv(eq(signals[n], s('last_' + n))) for n in payload)))
        reg(ch + '_held', 'bool', False, all_of(valid, inv(ready)))
        for n in payload: reg('last_' + n, types[n], bv(types[n]['bv'], 0), signals[n])
    handshake = {c: all_of(signals[c + 'valid'], signals[c + 'ready']) for c in CHANNELS}
    violations['b_requires_aw_w'] = all_of(signals['bvalid'], any_of(eq(s('aw_count'), bv(width, 0)), eq(s('w_count'), bv(width, 0))))
    violations['r_requires_ar'] = all_of(signals['rvalid'], eq(s('ar_count'), bv(width, 0)))
    violations['b_response_code'] = all_of(signals['bvalid'], eq(signals['bresp'], bv(2, 1)))
    violations['r_response_code'] = all_of(signals['rvalid'], eq(signals['rresp'], bv(2, 1)))
    overflow = []
    for ch, response in [('aw', 'b'), ('w', 'b'), ('ar', 'r')]:
        count = s(ch + '_count'); push = handshake[ch]; pop = handshake[response]
        overflow.append(all_of(push, inv(pop), eq(count, bv(width, cap))))
        # No wraparound, even on bad/out-of-scope traces.
        up = ite(expr('ult', count, bv(width, cap)), expr('add', count, bv(width, 1)), count)
        down = ite(eq(count, bv(width, 0)), count, expr('sub', count, bv(width, 1)))
        reg(ch + '_count', {'bv': width}, bv(width, 0), ite(eq(push, pop), count, ite(push, up, down)))
    # Pair independent accepted AW/W streams in FIFO order, not by cycle or
    # response arrival. Empty queues can consume the current handshake directly.
    pair_counts = {ch: s(ch + '_pair_count') for ch in ('aw', 'w')}
    available = {ch: any_of(inv(eq(pair_counts[ch], bv(width, 0))), handshake[ch]) for ch in pair_counts}
    pair = all_of(available['aw'], available['w'])
    heads = {}
    lanes = config['data_width'] // 8
    payload = {'aw': ('awaddr', config['address_width']), 'w': ('wstrb', lanes)}
    for ch, (field, bits) in payload.items():
        count = pair_counts[ch]; push = handshake[ch]
        value = signals[field]
        if ch == 'aw': value = expr('band', value, bv(bits, min(lanes - 1, (1 << bits) - 1)))
        heads[ch] = ite(eq(count, bv(width, 0)), value, s(ch + '_pair_0'))
        up = ite(expr('ult', count, bv(width, cap)), expr('add', count, bv(width, 1)), count)
        down = ite(eq(count, bv(width, 0)), count, expr('sub', count, bv(width, 1)))
        reg(ch + '_pair_count', {'bv': width}, bv(width, 0), ite(eq(push, pair), count, ite(push, up, down)))
        for index in range(cap):
            # Append before shifting; this also covers full pop+push and bypass.
            def appended(at):
                old = s(ch + '_pair_' + str(at)) if at < cap else bv(bits, 0)
                return ite(all_of(push, eq(count, bv(width, at))), value, old)
            reg(ch + '_pair_' + str(index), {'bv': bits}, bv(bits, 0), ite(pair, appended(index + 1), appended(index)))
    violations['write_address_strobe'] = all_of(pair, any_of(*(
        all_of(eq(heads['aw'], bv(config['address_width'], offset)),
               inv(eq(expr('band', heads['w'], bv(lanes, (1 << offset) - 1)), bv(lanes, 0))))
        for offset in range(1, min(lanes, 1 << config['address_width'])))))
    metadata = rules()
    reset_bad = {}
    for owner in ('manager', 'subordinate'):
        name = owner + '_reset_valid'
        reset_bad[name] = any_of(*(reset_signals[c + 'valid'] for c, (role, _) in CHANNELS.items() if role == owner))
        violations[name] = False
    env_names = [n for n, r in metadata.items() if config['role'] != 'link' and r['owner'] != config['role']]
    env_now = any_of(*(violations[n] for n in env_names))
    env_reset = any_of(*(reset_bad.get(n, False) for n in env_names))
    scope_now = any_of(*overflow)
    reg('environment_bad', 'bool', env_reset, any_of(s('environment_bad'), env_now))
    reg('scope_bad', 'bool', False, any_of(s('scope_bad'), scope_now))
    legal = all_of(inv(s('environment_bad')), inv(env_now), inv(s('scope_bad')))
    # Do NOT mask a same-edge protocol fault with capacity overflow.
    for name, info in metadata.items():
        raw = violations[name]
        reg('bad_' + name, 'bool', all_of(inv(env_reset), reset_bad.get(name, False)),
            any_of(s('bad_' + name), all_of(legal, raw)))
    return state, reset, nxt


def bind(document, config, signals, reset_signals, objective='guarantees'):
    """Compose observer contracts into a native v3 per-clock `tick` binding.

    Existing components/bindings remain checked. The first version deliberately
    requires a single unconditional tick; event-selective bindings are rejected.

    Default objective='guarantees' is CONDITIONAL: success does not establish a
    legal counterpart or in-capacity trace. Call bind on the ORIGINAL document
    separately with objective='environment' and objective='scope'. Environment
    checks counterpart rules; scope checks capacity on a legal counterpart prefix.
    Concrete replay state also exposes axi_environment_bad and axi_scope_bad
    (the latter is raw overflow, even outside a legal counterpart prefix).
    No objective establishes environment nonvacuity; provide a positive stimulus
    or a separate cover. Never interpret guarantee success alone as link success.
    """
    parameters(config)
    doc = copy.deepcopy(document)
    imp = doc.get('implementation', {})
    if doc.get('version') != 3 or doc.get('kind') != 'specification' or doc.get('operations') != {'tick': {}} or imp.get('operations') != {'tick': True}:
        raise ValueError('AXI binding requires native v3 with one unconditional tick operation')
    if objective not in ('guarantees', 'scope', 'environment'):
        raise ValueError('invalid AXI objective')
    if any(n.startswith('axi_') for n in imp['state']) or 'AxiChecked' in doc['compositions'] or any(n.startswith('Axi_') for n in doc['components']):
        raise ValueError('reserved AXI observer names collide')
    state, reset, nxt = monitor(config, signals, reset_signals)
    for key, extension in [('state', state), ('reset', reset), ('next', nxt)]: imp[key].update(extension)
    names = [n for n, r in rules().items() if config['role'] == 'link' or r['owner'] == config['role']]
    flags = ['bad_' + n for n in names] if objective == 'guarantees' else [objective + '_bad']
    members = [imp['composition']] if objective == 'guarantees' else []
    if objective != 'guarantees':
        imp['binding']['states'] = {}
    for flag in flags:
        name = 'Axi_' + flag
        doc['components'][name] = {'state': {'bad': 'bool'}, 'init': inv('s.bad'), 'invariant': inv('s.bad'), 'steps': {'tick': inv('n.bad')}, 'examples': {}}
        mapped = 's.axi_' + flag
        if objective == 'scope':
            mapped = all_of(mapped, inv('s.axi_environment_bad'))
        imp['binding']['states'][name] = {'bad': mapped}
        members.append(name)
    doc['compositions']['AxiChecked'] = {'members': members, 'examples': {}}
    imp['composition'] = 'AxiChecked'
    return doc


def trace_document(config, objective='guarantees'):
    """Standalone sampled-bus binding with the same objectives/caveats as bind.

    Generate separate documents for 'guarantees', 'environment', and 'scope'.
    Default guarantee success is conditional, not proof of a legal/in-scope trace.
    For concrete rows, axi4lite_reference.check_trace returns these dispositions
    separately and explicitly reports that environment nonvacuity is unchecked.
    """
    types = signal_types(config)
    base = {'version': 3, 'kind': 'specification', 'name': 'AXI4-Lite sampled contract',
            'inputs': {'rst': 'bool', **types}, 'observations': {}, 'operations': {'tick': {}},
            'components': {'Clock': {'state': {}, 'init': True, 'invariant': True, 'steps': {'tick': True}, 'examples': {}}},
            'compositions': {'Bus': {'members': ['Clock'], 'examples': {}}},
            'implementation': {'composition': 'Bus', 'reset_input': 'rst', 'state': {}, 'reset': {}, 'next': {}, 'wires': {},
                               'operations': {'tick': True}, 'binding': {'states': {'Clock': {}}, 'observations': {}}}}
    signals = {n: 'i.' + n for n in types}
    return bind(base, config, signals, signals, objective)
