"""Verify portable byte evidence; never execute an archived checker or proof."""
import argparse
import json
from pathlib import Path
from audit.rv32i_milestone.verify_evidence import verify_bundle


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--extract')
    parser.add_argument('--check-originals', action='store_true')
    args = parser.parse_args()
    print(json.dumps(verify_bundle(Path(__file__).with_name('evidence'), args.extract,
                                  args.check_originals), indent=2))

if __name__ == '__main__':
    main()
