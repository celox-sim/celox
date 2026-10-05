"""Fail-closed acceptance of fresh migrated-program diagnostics; not proof authority."""

def validate(report, metadata):
    expected = {p['id']: p for p in metadata['programs']}
    observed = {}
    for obligation in report['obligations']:
        for child in obligation.get('children', []):
            d = child.get('lemma_candidates')
            if d is None:
                continue
            if d['program'] in observed:
                raise ValueError('duplicate candidate-program diagnostic')
            observed[d['program']] = d
    if set(expected) != set(observed):
        raise ValueError('missing or unexpected migrated manual program')
    checked = applied = 0
    for name, program in expected.items():
        d = observed[name]
        if d['target_closed'] is not True or d['saved_reports_are_authority'] is not False or d.get('error') is not None:
            raise ValueError('migrated target was not closed by the live checker')
        if any(s['op'] == 'prove' for s in program['steps']):
            raise ValueError('legacy prove step escaped migration')
        claims = [s['id'] for s in program['steps'] if s['op'] == 'candidate']
        uses = [s['id'] for s in program['steps'] if s['op'] == 'use_candidate']
        if claims != [c['id'] for c in d['candidates']] or uses != [u['id'] for u in d['uses']]:
            raise ValueError('candidate or use-site coverage differs')
        if any(c['validity'] != 'established' or c['claim_true'] is not True or c['state'] not in ('checked','applied') for c in d['candidates']):
            raise ValueError('unproved candidate in migrated proof')
        if any(u['state'] != 'applied' for u in d['uses']):
            raise ValueError('use-context guard not established')
        checked += len(claims); applied += len(uses)
    return dict(programs=len(expected), checked_candidates=checked, checked_guard_uses=applied)


def validate_native_rv_sources(report, source):
    """Require the selected RV gate to exercise its native declarations."""
    expected = {('m_address', 'ir'), ('x_operand1', 'delivery'), ('x_operand1', 'equality')}
    found = set()
    for obligation in report['obligations']:
        for child in obligation.get('children', []):
            d = child.get('lemma_candidates', {})
            for row in d.get('candidates', []) + d.get('uses', []):
                key = (d.get('program'), row['id'])
                location = row.get('source') or ''
                if (key in expected and row.get('state') == 'applied'
                        and (row['id'] == 'equality' or row.get('usefulness') == 'target_closed')
                        and location.startswith(str(source) + ':')):
                    suffix = location[len(str(source)) + 1:].split(':')
                    if len(suffix) == 2 and all(v.isdigit() and int(v) > 0 for v in suffix):
                        found.add(key)
    if found != expected:
        raise ValueError('missing native RV lemma/use source coverage')
