"""Guarded encoded-state fixtures; no CPU model, proof hints or constrained inputs."""
import copy
from audit.equality_sharing.generate import alpha_rename, conjunction, op, word

FAMILIES = ('guarded_encoded_product', 'guarded_encoded_mac_shift')
POSITIVES = tuple(name for family in FAMILIES for name in (family, family+'_reversed'))


def expression(family, value, factor, bias, shift):
    result = op('mul', value, factor)
    if family == FAMILIES[1]:
        result = op('lshr', op('add', result, bias), shift)
    return result


def document(family, width=32):
    if family not in FAMILIES:
        raise ValueError(family)
    sort, zero = {'bv': width}, word(width, 0)
    inputs = {key: sort for key in ('seed', 'factor', 'bias', 'shift', 'inactive')}
    inputs.update(rst='bool', route='bool', pick='bool')
    def wrap(value, prefix):
        return expression(family, value, prefix+'factor', prefix+'bias', prefix+'shift')
    spec = {'state': {'encoded': sort, 'out': sort, 'active': 'bool'},
            'reset': {'encoded': op('ite', 'i.route', wrap('i.seed', 'i.'), 'i.inactive'),
                      'out': zero, 'active': 'i.route'},
            'next': {'encoded': 's.encoded', 'out': op('ite', 's.active', 's.encoded', zero),
                     'active': 's.active'}, 'outputs': {'allowed': True}}
    state = {key: sort for key in ('a', 'b', 'c', 'd', 'factor', 'bias', 'shift', 'out')}
    state['active'] = 'bool'
    reset = {'a': 'i.seed', 'b': zero, 'c': 'i.seed', 'd': zero,
             'factor': 'i.factor', 'bias': 'i.bias', 'shift': 'i.shift', 'out': zero,
             'active': 'i.route'}
    next_state = {key: 's.'+key for key in state}
    next_state['out'] = op('ite', 's.active', wrap(op('bxor', 's.c', 's.d'), 's.'), zero)
    impl = {'state': state, 'reset': reset, 'next': next_state, 'outputs': {'retire': True}}
    additive = op('add', 'impl.a', 'impl.b')
    facts = [op('eq', 'spec.out', 'impl.out'), op('eq', 'spec.active', 'impl.active'),
             op('implies', 'spec.active', conjunction([
                 op('eq', 'spec.encoded', wrap(additive, 'impl.')),
                 op('eq', additive, op('bxor', 'impl.c', 'impl.d'))]))]
    return {'version': 2, 'name': family, 'inputs': inputs, 'reset_input': 'rst',
            'spec': spec, 'impl': impl, 'binding': conjunction(facts),
            'commit': 'retire', 'can_step': 'allowed', 'progress': {'enabled': True, 'rank': word(1, 0)}}


def _forward_cases(width=32):
    for family in FAMILIES:
        original = document(family, width)
        yield original
        for kind in ('wrong_result', 'complement_wrong', 'guard_escape_wrong'):
            bad = copy.deepcopy(original)
            bad['name'] += '_'+kind
            value = bad['impl']['next']['out']
            if kind == 'wrong_result':
                bad['impl']['next']['out'] = op('add', value, word(width, 1))
            elif kind == 'complement_wrong':
                bad['impl']['next']['out'] = op('ite', 'i.pick', value, op('add', value, word(width, 1)))
            else:
                bad['spec']['next']['out'] = bad['spec']['next']['out'][2]
                bad['impl']['next']['out'] = value[2]
            yield bad


def cases(width=32):
    for doc in _forward_cases(width):
        yield doc
        mirrored = copy.deepcopy(doc)
        mirrored['name'] += '_reversed'
        # Reverse only the output equality; definitions and guards are identical.
        equality = mirrored['binding'][1]
        assert equality == ['eq', 'spec.out', 'impl.out']
        equality[1], equality[2] = equality[2], equality[1]
        yield mirrored
