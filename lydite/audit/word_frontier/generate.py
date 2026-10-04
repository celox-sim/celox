"""ISA-independent guarded word-bank transport fixtures with unconstrained inputs."""
import copy
from audit.equality_sharing.generate import alpha_rename, conjunction, op, word

FAMILIES = ('guarded_balanced_bank_product', 'guarded_priority_bank_mac_shift')


def read_bank(prefix, family):
    b = [prefix + 'bank' + str(i) for i in range(4)]
    if family == FAMILIES[0]:
        return op('ite', prefix + 'select0', op('ite', prefix + 'select1', b[0], b[1]),
                  op('ite', prefix + 'select1', b[2], b[3]))
    return op('ite', prefix + 'select0', b[0], op('ite', prefix + 'select1', b[1],
              op('ite', prefix + 'select2', b[2], b[3])))


def document(family, width=32):
    if family not in FAMILIES: raise ValueError(family)
    sort = {'bv': width}
    inputs = {'rst': 'bool', 'route': 'bool', 'pick': 'bool', 'factor': sort,
              'bias': sort, 'shift': sort, 'fallback': sort, 'inactive': sort}
    inputs.update({'bank'+str(i): sort for i in range(4)})
    inputs.update({'select'+str(i): 'bool' for i in range(3)})
    ss = {'value': sort, 'route': 'bool', 'out': sort}
    ins = {'bank'+str(i): sort for i in range(4)}
    ins.update({'select'+str(i): 'bool' for i in range(3)})
    ins.update(route='bool', out=sort)
    sr = {'value': op('ite', 'i.route', read_bank('i.', family), 'i.inactive'),
          'route': 'i.route', 'out': word(width, 0)}
    ir = {k: 'i.'+k for k in ins}
    ir['out'] = word(width, 0)
    sn = {k: 's.'+k for k in ss}
    inn = {k: 's.'+k for k in ins}
    def wrap(x):
        value = op('mul', x, 'i.factor')
        if family == FAMILIES[1]: value = op('lshr', op('add', value, 'i.bias'), 'i.shift')
        return op('ite', 's.route', value, 'i.fallback')
    sn['out'], inn['out'] = wrap('s.value'), wrap(read_bank('s.', family))
    # An equivalent zero-XOR relation prevents directly retrieving a stored
    # word equality; the proposed full-word cut still needs fresh proof.
    relation = op('eq', op('bxor', 'spec.value', read_bank('impl.', family)), word(width, 0))
    return {'version': 2, 'name': family, 'inputs': inputs, 'reset_input': 'rst',
            'spec': {'state': ss, 'reset': sr, 'next': sn, 'outputs': {'allowed': True}},
            'impl': {'state': ins, 'reset': ir, 'next': inn, 'outputs': {'retire': True}},
            'binding': conjunction([op('eq','spec.out','impl.out'), op('eq','spec.route','impl.route'),
                                    op('implies', 'spec.route', relation)]),
            'commit': 'retire', 'can_step': 'allowed',
            'progress': {'enabled': True, 'rank': word(1, 0)}}


def cases(width=32):
    for family in FAMILIES:
        original = document(family, width)
        yield original
        bad = copy.deepcopy(original)
        bad['name'] += '_wrong_result'
        bad['impl']['next']['out'] = op('add', bad['impl']['next']['out'], word(width, 1))
        yield bad
        bad = copy.deepcopy(original)
        bad['name'] += '_complement_wrong'
        value = bad['impl']['next']['out']
        bad['impl']['next']['out'] = op('ite', 'i.pick', value, op('add', value, word(width, 1)))
        yield bad
