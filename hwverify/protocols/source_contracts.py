"""Reusable native v3 source-refinement templates, without protocol latency claims.

Bindings are explicit implementation expressions. FIFO slots are actual RTL state,
not a monitor's invented response identities. The caller supplies independent
request semantics and must validate source mappings. Each returned document checks
one obligation, preserving the original specification without assuming siblings.
"""
import copy
import re
from protocols.axi4lite import all_of, any_of, inv, eq, ite, bv, expr, substitute


def _attach(document, name, types, bindings, initial, invariant, step):
    doc = copy.deepcopy(document); imp = doc['implementation']
    if doc.get('version') != 3 or doc.get('operations') != {'tick': {}} or imp['operations'] != {'tick': True}:
        raise ValueError('source contracts require native v3 unconditional tick')
    if not isinstance(name, str) or not re.fullmatch(r'[A-Za-z_][A-Za-z_0-9]*', name):
        raise ValueError('contract name must be an identifier')
    composition = name + '_checked'
    if name in doc['components'] or composition in doc['compositions']:
        raise ValueError('contract name collision')
    if set(types) != set(bindings): raise ValueError('contract binding fields mismatch')
    doc['components'][name] = {'state': types, 'init': initial, 'invariant': invariant,
                               'steps': {'tick': step}, 'examples': {}}
    doc['compositions'][composition] = {'members': [imp['composition'], name], 'examples': {}}
    imp['composition'] = composition; imp['binding']['states'][name] = bindings
    return doc


def fifo_read(document, name, capacity, request_width, response_width, bindings, request_valid, request_payload, response_ready, response_function):
    """Return storage, capacity, response and stall obligations over a real FIFO.

    bindings: count, slot_0..slot_N, ready, valid, data (state-only expressions).
    response_function: canonical expression using only the placeholder `request`
    and literals/operators, independently authored from the DUT output logic.
    Acceptance appends; response handshake retires; simultaneous pop/push is legal,
    including at capacity. No empty bypass or eventual-response claim. Inactive
    slot bits are unconstrained. Repeated payloads remain distinct queue positions,
    but equal observable responses cannot establish physical transaction identity.
    """
    for value, allowed in [(capacity, range(1, 17)), (request_width, range(1, 65)), (response_width, range(1, 65))]:
        if type(value) is not int or value not in allowed: raise ValueError('unsupported FIFO dimensions')
    def check_function(term):
        if isinstance(term, str) and term != 'request': raise ValueError('response function may reference only request')
        if isinstance(term, list):
            for argument in term[1:]: check_function(argument)
    check_function(response_function)
    width = capacity.bit_length(); zero = bv(width, 0); one = bv(width, 1)
    types = {'count': {'bv': width}, 'ready': 'bool', 'valid': 'bool', 'data': {'bv': response_width},
             **{'slot_' + str(i): {'bv': request_width} for i in range(capacity)}}
    push = all_of(request_valid, 's.ready'); pop = all_of('s.valid', response_ready)
    empty = eq('s.count', zero); full = eq('s.count', bv(width, capacity))
    count_next = ite(eq(push, pop), 's.count', ite(push, expr('add', 's.count', one), expr('sub', 's.count', one)))
    remaining = ite(pop, expr('sub', 's.count', one), 's.count')
    storage = [eq('n.count', count_next), inv(all_of(pop, empty))]
    for i in range(capacity):
        shifted = ('s.slot_' + str(i + 1)) if i + 1 < capacity else bv(request_width, 0)
        value = ite(all_of(push, eq(remaining, bv(width, i))), request_payload, ite(pop, shifted, 's.slot_' + str(i)))
        storage.append(ite(expr('ult', bv(width, i), 'n.count'), eq('n.slot_' + str(i), value), True))
    bounded = expr('ule', 's.count', bv(width, capacity))
    expected = substitute(response_function, {'request': 's.slot_0'})
    offered = ite('s.valid', all_of(inv(empty), eq('s.data', expected)), True)
    obligations = {
        'storage': (eq('s.count', zero), bounded, all_of(*storage)),
        'capacity': (eq('s.count', zero), bounded, inv(all_of(push, full, inv(pop)))),
        'response': (inv('s.valid'), offered, True),
        'stall': (inv('s.valid'), True, ite(all_of('s.valid', inv(response_ready)), all_of('n.valid', eq('n.data', 's.data')), True)),
    }
    return {key: _attach(document, name + '_' + key, types, bindings, *parts) for key, parts in obligations.items()}


def idle_offer_step(document, name, bindings, offer, completion):
    """Resource accounting and one-step launch, independently checked.

    bindings are busy and one or more valid_N actual state expressions. Availability
    means !busy and no pending offer. The application defines offer/completion;
    these cannot be deduced from bus pins. Completion wins busy update, matching
    the declared adapter contract. The one-step launch is an application contract,
    NOT an AXI deadline, a fairness condition, or all-history READY independence.
    """
    valids = ['valid_' + str(i) for i in range(len(bindings) - 1)]
    if not valids or set(bindings) != {'busy', *valids}:
        raise ValueError('offer binding needs busy and contiguous valid_N fields')
    if 'busy' not in bindings: raise ValueError('offer binding needs busy')
    types = {k: 'bool' for k in bindings}
    available = all_of(inv('s.busy'), inv(any_of(*('s.' + k for k in valids))))
    launched = all_of(*('n.' + k for k in valids))
    busy_next = ite(completion, False, any_of('s.busy', offer))
    # Source offer is accepted only when not busy; busy OR offer is equivalent.
    return {
        'resource': _attach(document, name + '_resource', types, bindings, inv('s.busy'), True, eq('n.busy', busy_next)),
        'launch': _attach(document, name + '_launch', types, bindings, all_of(inv('s.busy'), *(inv('s.' + k) for k in valids)), True,
                          ite(all_of(offer, available), launched, True)),
    }


def memory_write(document, name, address_width, data_width, locations, initial, bindings, inputs):
    """Single outstanding AW/W pair, explicit scalar memory and actual effect event.

    Six independent obligations bind actual request slots, actual apply/applied
    signals, memory cells and response pins. Effects have no invented deadline.
    Byte lanes are little-endian; addresses select aligned bus-width words.
    Unmapped addresses have no write effect and respond DECERR; reads return zero.
    Readback is an explicit one-slot, one-edge, read-before-write application
    contract. A pending response must retire before another read is accepted;
    simultaneous retirement/refill is allowed. Buffered read designs need a
    separate FIFO contract, not this one-slot application template.
    Neither response IDs nor effect origins are assigned by a monitor.
    """
    if type(data_width) is not int or data_width not in (32, 64): raise ValueError('memory data width must be 32 or 64')
    lanes = data_width // 8
    if type(address_width) is not int or not lanes.bit_length() - 1 <= address_width <= 64: raise ValueError('unsupported memory address width')
    if not isinstance(locations, list) or not 1 <= len(locations) <= 16 or any(type(x) is not int or not 0 <= x < 1 << address_width or x % lanes for x in locations) or len(set(locations)) != len(locations):
        raise ValueError('memory locations must be distinct aligned addresses, 1..16 cells')
    if not isinstance(initial, list) or len(initial) != len(locations) or any(type(x) is not int or not 0 <= x < 1 << data_width for x in initial): raise ValueError('invalid explicit initial memory')
    if set(inputs) != {'aw_valid','aw_address','w_valid','w_data','w_strobes','b_ready','ar_valid','ar_address','r_ready'}: raise ValueError('memory input binding fields mismatch')
    types = {k: 'bool' for k in ['aw_pending','w_pending','applied','apply','aw_ready','w_ready','b_valid','read_ready','read_valid']}
    types.update({'address': {'bv': address_width}, 'data': {'bv': data_width}, 'strobes': {'bv': lanes},
                  'b_response': {'bv': 2}, 'read_data': {'bv': data_width}, 'read_response': {'bv': 2},
                  **{'cell_' + str(i): {'bv': data_width} for i in range(len(locations))}})
    push_a = all_of(inputs['aw_valid'], 's.aw_ready'); push_w = all_of(inputs['w_valid'], 's.w_ready')
    retire = all_of('s.b_valid', inputs['b_ready']); apply = 's.apply'
    requests = [eq('n.' + field, any_of(all_of('s.' + field, inv(retire)), push)) for field, push in [('aw_pending', push_a), ('w_pending', push_w)]]
    for field, push, value in [('address', push_a, inputs['aw_address']), ('data', push_w, inputs['w_data']), ('strobes', push_w, inputs['w_strobes'])]:
        owner = 'aw_pending' if field == 'address' else 'w_pending'
        requests.append(ite('n.' + owner, eq('n.' + field, ite(push, value, 's.' + field)), True))
    mask = bv(data_width, 0)
    for lane in range(lanes):
        enabled = inv(eq(expr('band', 's.strobes', bv(lanes, 1 << lane)), bv(lanes, 0)))
        mask = expr('bor', mask, ite(enabled, bv(data_width, 255 << (lane * 8)), bv(data_width, 0)))
    def selected(address, location):
        return eq(expr('band', address, bv(address_width, ((1 << address_width) - 1) & ~(lanes - 1))), bv(address_width, location))
    write_hits = [selected('s.address', address) for address in locations]
    effects = []; reset_cells = []
    read_value = bv(data_width, 0)
    for i, address in reversed(list(enumerate(locations))):
        cell = 'cell_' + str(i)
        merged = expr('bor', expr('band', 's.' + cell, expr('bnot', mask)), expr('band', 's.data', mask))
        effects.append(eq('n.' + cell, ite(all_of(apply, write_hits[i]), merged, 's.' + cell)))
        reset_cells.append(eq('s.' + cell, bv(data_width, initial[i])))
        read_value = ite(selected(inputs['ar_address'], address), 's.' + cell, read_value)
    response = ite('s.b_valid', all_of('s.applied','s.aw_pending','s.w_pending',eq('s.b_response', ite(any_of(*write_hits),bv(2,0),bv(2,3)))), True)
    read_push = all_of(inputs['ar_valid'], 's.read_ready')
    read_code = ite(any_of(*(selected(inputs['ar_address'], a) for a in locations)), bv(2,0), bv(2,3))
    obligations = {
        'requests': (all_of(inv('s.aw_pending'),inv('s.w_pending')), True, all_of(*requests)),
        'capacity': (True, True, inv(any_of(all_of(push_a,'s.aw_pending',inv(retire)),all_of(push_w,'s.w_pending',inv(retire)),all_of(read_push,'s.read_valid',inv(inputs['r_ready']))))),
        'effects': (all_of(*reset_cells), True, all_of(*effects)),
        'completion': (inv('s.applied'), True, all_of(ite(apply,all_of('s.aw_pending','s.w_pending',inv('s.applied')),True),
                            ite(retire,'s.applied',True),eq('n.applied',ite(retire,False,any_of('s.applied',apply))))),
        'response': (inv('s.b_valid'), response, ite(all_of('s.b_valid',inv(inputs['b_ready'])),all_of('n.b_valid',eq('n.b_response','s.b_response')),True)),
        'readback': (inv('s.read_valid'), True, all_of(ite(all_of('s.read_valid',inv(inputs['r_ready'])),all_of('n.read_valid',eq('n.read_data','s.read_data'),eq('n.read_response','s.read_response')),True),eq('n.read_valid',any_of(read_push,all_of('s.read_valid',inv(inputs['r_ready'])))),
                           ite('n.read_valid',all_of(eq('n.read_data',ite(read_push,read_value,'s.read_data')),eq('n.read_response',ite(read_push,read_code,'s.read_response'))),True))),
    }
    return {key: _attach(document, name + '_' + key, types, bindings, *parts) for key, parts in obligations.items()}
