"""Load legacy reports or a schema-4 index and its test-group files."""
import json
import re
from pathlib import Path


def read_report(path):
    path = Path(path)
    report = json.loads(path.read_text())
    if report.get("schema_version") != 4:
        return report
    if "cases" in report:
        raise ValueError("split index contains cases")
    identifier = re.compile(r"[A-Za-z0-9_-]+", re.ASCII)
    if not identifier.fullmatch(path.stem):
        raise ValueError("invalid report filename")
    files = report.get("case_files")
    if (not isinstance(files, list) or not files
            or not all(isinstance(file, str) for file in files)
            or files != sorted(set(files))):
        raise ValueError("case_files must be nonempty, sorted and unique")
    cases, names = [], set()
    for file in files:
        prefix = f"{path.stem}/"
        if not file.startswith(prefix) or not file.endswith(".json"):
            raise ValueError(f"invalid case file: {file}")
        group = file[len(prefix):-5]
        if not identifier.fullmatch(group):
            raise ValueError(f"invalid case file: {file}")
        shard = json.loads((path.parent / file).read_text())
        rows = shard.get("cases")
        if not isinstance(rows, list) or not rows:
            raise ValueError(f"case file has no cases: {file}")
        for row in rows:
            name = row.get("name")
            if not isinstance(name, str) or not name.startswith(f"{group}::"):
                raise ValueError(f"case does not belong in {file}")
            if name in names:
                raise ValueError(f"duplicate case: {name}")
            names.add(name)
            cases.append(row)
    del report["case_files"]
    report.update(schema_version=3, cases=sorted(cases, key=lambda row: row["name"]))
    return report
