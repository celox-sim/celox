"""Verify portable byte evidence; never execute an archived checker or proof."""
import argparse
import json
from pathlib import Path
from audit.rv32i_milestone.verify_evidence import verify_bundle


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--extract', help='New sibling-directory prefix for the two evidence bundles')
    parser.add_argument('--check-originals', action='store_true')
    args = parser.parse_args()
    results = {}
    for name in ('evidence', 'orientation-evidence'):
        destination = args.extract + '-' + name if args.extract else None
        results[name] = verify_bundle(Path(__file__).with_name(name), destination,
                                      args.check_originals)
    print(json.dumps(results, indent=2))

if __name__ == '__main__':
    main()
