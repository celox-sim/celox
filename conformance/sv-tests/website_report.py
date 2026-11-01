#!/usr/bin/env python3
"""Publish only complete measurements, with reproducible provenance."""
import argparse
import csv
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import subprocess


def publish(source, destination, celox_revision=None):
    report = json.loads(source.read_text())
    if report.get('complete') is not True:
        raise ValueError('cannot publish incomplete verification results')
    # Keep bulky per-case excerpts and local log paths in the raw artifacts.
    for states in report.get('groups', {}).values():
        for counts in states.values():
            for field in ('by_failure_reason', 'by_failure_diagnostic', 'negative_pass_diagnostics'):
                counts.pop(field, None)
    if source.name.endswith('.summary.json'):
        raw = source.with_name(source.name.removesuffix('.summary.json') + '.tsv')
        if raw.exists():
            with raw.open(newline='') as stream:
                report['cases'] = [{key: row.get(key, '') for key in
                                    ('test', 'state_mode', 'result', 'mode', 'tags', 'failure_category', 'diagnostic', 'input_issue')}
                                   for row in csv.DictReader(stream, delimiter='\t') if row.get('group') == 'cores']
    report['driver_profile'] = os.environ.get('CELOX_SV_DRIVER_PROFILE', 'debug')
    report['measured_at'] = datetime.now(timezone.utc).isoformat()
    report['celox_revision'] = celox_revision or subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
    report['run_url'] = (f"https://github.com/{os.environ['GITHUB_REPOSITORY']}/actions/runs/{os.environ['GITHUB_RUN_ID']}"
                         if 'GITHUB_RUN_ID' in os.environ else None)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('destination', type=Path)
    parser.add_argument('--celox-revision', help='revision of a driver built in a separate checkout')
    args = parser.parse_args()
    publish(args.source, args.destination, args.celox_revision)
