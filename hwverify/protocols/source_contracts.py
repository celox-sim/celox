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
