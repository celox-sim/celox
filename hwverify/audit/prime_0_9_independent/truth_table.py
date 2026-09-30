"""Check expectation AND/OR and prime semantics against concrete arithmetic."""
import argparse
import itertools
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]

def fixture():
    text = ['specification "Independent expectation truth table"']
    expected = {}
    for name, body in [('Blocks', 'any { all { expect advances; expect allowed; } expect holds; }'),
                       ('Expression', 'expect (advances && allowed) || holds;')]:
        text += [f'spec {name}(input amount: bv<2>, input enable: bool, output count: bv<2>) {{',
                 '  state value: bv<2>;', '  init true;', '  invariant count == value;',
                 '  expectation allowed { expect value\' != 3u2; }',
                 '  expectation advances { expect delta; }',
                 '  expectation delta { expect value\' == value + amount; }',
                 '  expectation holds { all { expect enable; expect value\' == value; } }',
                 '  operation choose { ' + body + ' expect count\' == value\'; }']
        for before, amount, enable, after in itertools.product(range(4), range(4), [False, True], range(4)):
            admitted = (after == (before + amount) % 4 and after != 3) or (enable and after == before)
            case = f'b{before}_a{amount}_e{int(enable)}_n{after}'
            expected[(name, case)] = admitted
            text += [f'  example {case} {{', f'    expect {"positive" if admitted else "negative"};',
                     f'    initial {{ count = {before}u2; }}',
                     f'    trace {{ choose {{ inputs {{ amount = {amount}u2; enable = {str(enable).lower()}; }} observe {{ count = {after}u2; }} }} }}', '  }']
        text += ['}']
    return '\n'.join(text) + '\n', expected

if __name__ == '__main__':
    p = argparse.ArgumentParser(); p.add_argument('--binary', type=Path, required=True); p.add_argument('--z3', required=True)
    p.add_argument('--out', type=Path, default=ROOT / 'results/prime_0_9_truth_table'); a = p.parse_args()
    a.out.mkdir(parents=True, exist_ok=True)
    source, expected = fixture(); path = a.out / 'truth_table.hwv'; path.write_text(source)
    run = subprocess.run([str(a.binary.resolve()), str(path), '--z3', a.z3, '--out', str(a.out / 'run')], capture_output=True, text=True, timeout=180)
    (a.out / 'run.stdout').write_text(run.stdout); (a.out / 'run.stderr').write_text(run.stderr)
    assert run.returncode == 0, (run.returncode, run.stdout, run.stderr)
    report = json.loads((a.out / 'run/report.json').read_text())
    actual = {(ex['target'], ex['example']): ex for ex in report['examples']}
    assert set(actual) == set(expected), (len(actual), len(expected))
    for key, admitted in expected.items():
        assert actual[key]['admitted'] == admitted and actual[key]['status'] == 'passed', (key, actual[key])
    result = {'status': 'pass', 'cases': len(expected), 'positive': sum(expected.values()), 'negative': len(expected) - sum(expected.values()), 'report_status': report['status']}
    (ROOT / 'audit/prime_0_9_independent/truth_table_results.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result))
