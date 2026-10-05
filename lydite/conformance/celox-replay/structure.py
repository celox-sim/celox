"""Conservative source-variable dependency graph, before behavioral simplification.

This is a deliberately small, total-coverage Veryl source parser. The original
Veryl frontend must also compile the identical design. Anything outside this
subset makes the entire graph unsupported: never infer absence from missing edges.
"""
import bisect
import hashlib
import json
import re

TOKEN = re.compile(r"\s+|//[^\n]*|/\*[\s\S]*?\*/|[0-9]+\s*'[sS]?[bBoOdDhH][0-9a-fA-F_xXzZ]+|[0-9]+|[A-Za-z_][A-Za-z_0-9]*|&&|\|\||==|!=|<=|>=|<<|>>|[{}():;,<>.=+*/%&|^!~?-]")
IDENT = re.compile(r'[A-Za-z_][A-Za-z_0-9]*\Z')

class Unsupported(ValueError): pass

class Parser:
    def __init__(self, source):
        if len(source['text']) > 1024 * 1024: raise Unsupported('source graph input budget exceeded')
        lines = [0] + [m.end() for m in re.finditer('\n', source['text'])]
        self.source = source; self.tokens = []; self.index = 0; pos = 0
        for m in TOKEN.finditer(source['text']):
            if m.start() != pos: raise Unsupported('unsupported source token')
            token = m.group(); pos = m.end()
            if "'" in token and re.search('[xXzZ]', token): raise Unsupported('X/Z source literals unsupported')
            if not token.isspace() and not token.startswith(('//', '/*')):
                line = bisect.bisect_right(lines, m.start())
                self.tokens.append((token, {'file': source['path'], 'line': line, 'column': m.start() - lines[line - 1] + 1}))
        if pos != len(source['text']): raise Unsupported('unsupported trailing source token')
    def peek(self): return self.tokens[self.index][0] if self.index < len(self.tokens) else None
    def take(self, expected=None):
        if self.peek() is None: raise Unsupported('unexpected end of source')
        token = self.tokens[self.index]; self.index += 1
        if expected is not None and token[0] != expected: raise Unsupported(f'expected {expected}, found {token[0]}')
        return token
    def name(self):
        value = self.take()[0]
        if not IDENT.fullmatch(value): raise Unsupported('plain identifier required')
        return value
    def ty(self):
        kind = self.name(); width = 1
        if kind not in ('bit', 'logic', 'clock'): raise Unsupported('only scalar bit/logic and positive-edge clock declarations supported')
        if self.peek() == '<':
            self.take('<'); text = self.take()[0]; self.take('>')
            if kind == 'clock' or not text.isdecimal(): raise Unsupported('literal scalar width required')
            width = int(text)
        if not 1 <= width <= 64: raise Unsupported('scalar width outside 1..64')
        return kind, width
    def expression(self, until):
        refs = set(); depth = 0; consumed = False
        while self.peek() is not None:
            value = self.peek()
            if depth == 0 and value in until: break
            self.take(); consumed = True
            if value == '(':
                depth += 1
            elif value == ')':
                depth -= 1
                if depth < 0: raise Unsupported('unbalanced expression')
            elif IDENT.fullmatch(value):
                if value in ('if', 'else', 'as', 'inside', 'outside') or self.peek() in ('(', '.', ':'):
                    raise Unsupported('function, cast, scoped or conditional expression unsupported')
                refs.add(value)
            elif value in ('{', '}', '[', ']', '.', ':', ';', ',', '=', '?'):
                raise Unsupported('unsupported expression form')
        if not consumed or depth != 0: raise Unsupported('empty/unbalanced expression')
        return refs
    def statements(self, module, sequential=False, guards=None):
        guards = set() if guards is None else guards
        self.take('{')
        while self.peek() != '}':
            if self.peek() == 'if':
                self.take('if'); condition = self.expression({'{'})
                self.statements(module, sequential, guards | condition)
                if self.peek() == 'else':
                    self.take('else')
                    # Nested else-if uses the same parser without consuming a block.
                    if self.peek() == 'if':
                        raise Unsupported('else-if unsupported; use explicit nested else block')
                    self.statements(module, sequential, guards | condition)
            else:
                target, location = self.take()
                if not IDENT.fullmatch(target): raise Unsupported('plain assignment destination required')
                self.take('='); refs = self.expression({';'}); self.take(';')
                module['assignments'].append((target, refs, guards.copy(), sequential, location))
        self.take('}')
    def modules(self):
        result = {}
        while self.peek() is not None:
            self.take('module'); name = self.name(); self.take('(')
            m = {'ports': {}, 'vars': {}, 'assignments': [], 'instances': []}
            while self.peek() != ')':
                n, loc = self.take(); self.take(':'); direction = self.take()[0]; kind, width = self.ty()
                if not IDENT.fullmatch(n) or direction not in ('input', 'output') or n in m['vars']: raise Unsupported('unsupported/duplicate port')
                m['vars'][n] = {'kind': direction.title(), 'type_kind': kind.title(), 'width': width, 'location': loc}
                m['ports'][n] = direction
                if self.peek() != ')': self.take(',')
            self.take(')'); self.take('{')
            while self.peek() != '}':
                kind = self.take()[0]
                if kind == 'var':
                    n, loc = self.take(); self.take(':'); ty, width = self.ty(); self.take(';')
                    if not IDENT.fullmatch(n) or n in m['vars']: raise Unsupported('duplicate/unsupported variable')
                    m['vars'][n] = {'kind': 'Variable', 'type_kind': ty.title(), 'width': width, 'location': loc}
                elif kind == 'assign':
                    target, loc = self.take(); self.take('='); refs = self.expression({';'}); self.take(';')
                    if not IDENT.fullmatch(target): raise Unsupported('unsupported assign destination')
                    m['assignments'].append((target, refs, set(), False, loc))
                elif kind == 'always_comb': self.statements(m)
                elif kind == 'always_ff':
                    self.take('('); clock = self.name(); self.take(')')
                    if clock not in m['vars'] or m['vars'][clock]['type_kind'] != 'Clock': raise Unsupported('unsupported sequential clock/reset')
                    self.statements(m, sequential=True)
                elif kind == 'inst':
                    instance, loc = self.take(); self.take(':'); target = self.name(); self.take('('); connections = {}
                    while self.peek() != ')':
                        formal = self.name(); self.take(':'); actual = self.name()
                        if formal in connections: raise Unsupported('duplicate instance connection')
                        connections[formal] = actual
                        if self.peek() != ')': self.take(',')
                    self.take(')'); self.take(';')
                    m['instances'].append((instance, target, connections, loc))
                else: raise Unsupported('unsupported module declaration: ' + kind)
            self.take('}')
            if name in result: raise Unsupported('duplicate module')
            result[name] = m
        return result


def extract(design, compiled):
    """Return complete graph or explicit unsupported; requires identical compiled source.

    Whole-signal syntactic data/control dependencies, including dead/constant
    branches and cancelling operands. No combinational feasibility claim.
    """
    result = {'version': 1, 'status': 'unsupported', 'scope': 'elaborated_source_variable_graph',
              'design_sha256': hashlib.sha256(json.dumps(design, sort_keys=True, separators=(',', ':')).encode()).hexdigest(),
              'nodes': {}, 'edges': [], 'sequential_cuts': [], 'coverage': [], 'reasons': []}
    try:
        if design.get('four_state') is not False or compiled.get('four_state') is not False or compiled.get('status') != 'compiled_only_not_verified' or compiled.get('allowed_diagnostics'): raise Unsupported('validated two-state frontend artifact required')
        modules = {}
        for source in design['sources']:
            for name, m in Parser(source).modules().items():
                if name in modules: raise Unsupported('duplicate source module')
                modules[name] = m
        def elaborate(name, path, active):
            if name not in modules or name in active or len(active) > 32: raise Unsupported('opaque, recursive or deep hierarchy')
            m = modules[name]; prefix = path + '.' if path else ''
            result['coverage'].append({'instance': path, 'module': name})
            for n, meta in m['vars'].items():
                key = prefix + n
                if key in result['nodes']: raise Unsupported('ambiguous elaborated variable')
                result['nodes'][key] = {**meta, 'top_port': not path and n in m['ports']}
            for target, refs, guards, sequential, loc in m['assignments']:
                if target not in m['vars'] or any(n not in m['vars'] for n in refs | guards): raise Unsupported('unresolved assignment reference')
                for source in sorted(refs | guards):
                    edge = {'from': prefix + source, 'to': prefix + target, 'kind': 'control' if source in guards else 'data', 'location': loc}
                    result['sequential_cuts' if sequential else 'edges'].append(edge)
            seen = set()
            for instance, child, connections, loc in m['instances']:
                if instance in seen or instance in m['vars'] or child not in modules: raise Unsupported('ambiguous or opaque instance')
                seen.add(instance)
                if set(connections) != set(modules[child]['ports']): raise Unsupported('incomplete hierarchy port coverage')
                child_path = prefix + instance
                elaborate(child, child_path, active | {name})
                for formal, actual in connections.items():
                    if actual not in m['vars']: raise Unsupported('unresolved instance connection')
                    a, b = prefix + actual, child_path + '.' + formal
                    if result['nodes'][a]['width'] != result['nodes'][b]['width']: raise Unsupported('connection width mismatch')
                    if modules[child]['ports'][formal] == 'output': a, b = b, a
                    result['edges'].append({'from': a, 'to': b, 'kind': 'connection', 'location': loc})
        elaborate(design['top'], '', set())
        # Attest all source variables and instance paths against official typed
        # frontend reflection. No generated/indexed/interface paths in this subset.
        reflected = {}
        reflected_instances = {''}
        for signal in compiled['signals']:
            segments = []
            for item in signal['instances']:
                if not isinstance(item, list) or len(item) != 2 or item[1] != 0 or not IDENT.fullmatch(item[0]):
                    raise Unsupported('unsupported frontend instance path')
                segments.append(item[0])
            reflected_instances.add('.'.join(segments))
            if any(not IDENT.fullmatch(n) for n in signal['path']): raise Unsupported('unsupported frontend variable path')
            key = '.'.join(segments + signal['path'])
            if key in reflected: raise Unsupported('ambiguous frontend alias')
            reflected[key] = signal
        if reflected_instances != {c['instance'] for c in result['coverage']}:
            raise Unsupported('missing instance coverage')
        for name, node in result['nodes'].items():
            s = reflected.get(name)
            if s is None or s['metadata']['array_dims'] or s['metadata']['width'] != node['width'] or s['metadata']['type_kind'] != node['type_kind']:
                raise Unsupported('source/frontend variable coverage mismatch: ' + name)
            if node['kind'] in ('Input', 'Output') and s['kind'] != node['kind']: raise Unsupported('port direction mismatch')
        if set(reflected) != set(result['nodes']): raise Unsupported('uncovered frontend variables')
        result['status'] = 'complete'
    except (Unsupported, KeyError, TypeError, RecursionError) as error:
        result['reasons'].append(str(error))
    return result
