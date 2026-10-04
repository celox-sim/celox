"""Verify the exact-byte phase-2 replay bundle; no archived programs are run."""
import argparse
import json
from pathlib import Path

from audit.rv32i_milestone.verify_evidence import verify_bundle


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--extract", metavar="NEW_EXTERNAL_DIRECTORY")
    parser.add_argument("--check-originals", action="store_true")
    args = parser.parse_args()
    result = verify_bundle(Path(__file__).resolve().parent / "evidence",
                           args.extract, args.check_originals)
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
