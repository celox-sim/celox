"""Verify the nonbinary phase-3 publication evidence without running tools."""
import argparse
import json
from pathlib import Path

from audit.rv32i_milestone.verify_evidence import verify_bundle


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--extract", metavar="NEW_EXTERNAL_DIRECTORY")
    parser.add_argument("--check-originals", action="store_true")
    args = parser.parse_args()
    print(json.dumps(verify_bundle(Path(__file__).resolve().parent / "evidence",
                                  args.extract, args.check_originals), indent=2))


if __name__ == "__main__":
    main()
