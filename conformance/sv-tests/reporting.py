#!/usr/bin/env python3
"""Classify sv-tests diagnostics without changing the official runner verdicts."""

import argparse
from collections import Counter
import csv
import json
from pathlib import Path
import re
import sys

CLASSIFICATION_VERSION = 1
FIELDS = ("failure_category", "diagnostic", "evidence", "negative_test")
BASE_FIELDS = ("test", "state_mode", "result", "mode", "tags", "group",
               "input_issue", "rc", "tool_success", "log")


def classify(row, log):
    """Evidence labels, not a second verdict evaluator or a proof of root cause."""
    meta, separator, body = log.partition("\n\n")
    metadata = dict(line.split(": ", 1) for line in meta.splitlines() if ": " in line) if separator else {}
    # BaseRunner puts the invocation on the first line, except on timeout.
    lines = body.splitlines() if separator else []
    if lines and not lines[0].startswith("Timeout: > "):
        lines = lines[1:]
    text = re.sub(r"\x1b\[[0-9;]*m", "", "\n".join(lines))
    rc = metadata.get("rc", row.get("rc", ""))
    success = metadata.get("tool_success", row.get("tool_success", ""))
    negative = metadata.get("should_fail", "") == "1"
    category, diagnostic = "unclassified", "unrecognized_diagnostic"
    evidence = next((line.strip() for line in text.splitlines() if line.strip()), "no driver diagnostic")
    if not log:
        diagnostic, evidence = "missing_log", "official log is unavailable"
    elif row.get("input_issue"):
        category, diagnostic, evidence = "input_issue", "reviewed_upstream_input", row["input_issue"]
    elif rc == "71" and re.search(r"^Timeout: > \d+s$", text, re.MULTILINE):
        category, diagnostic = "timeout", "runner_timeout"
    elif re.search(r"^thread .* panicked at", text, re.MULTILINE):
        category, diagnostic = "crash", "rust_panic"
        evidence = re.search(r"^thread .* panicked at.*$", text, re.MULTILINE)[0]
    elif re.fullmatch(r"-?\d+", rc) and (int(rc) < 0 or int(rc) >= 126):
        category, diagnostic, evidence = "crash", "abnormal_exit", f"driver return code {rc}"
    elif row["result"] == "fail" and success == "1" and negative:
        category, diagnostic, evidence = "unexpected_success", "negative_test_succeeded", "negative test: driver succeeded"
    elif row["result"] == "fail" and success == "1" and metadata.get("should_fail") == "0" and row.get("mode") == "simulation":
        category, diagnostic, evidence = "assertion_failure", "official_output_validation", "official simulation output validation failed"
    elif re.search(r"^unsupported_simulator_parser$|^Unsupported SystemVerilog construct:", text, re.MULTILINE):
        category, diagnostic = "unsupported", "unsupported_construct"
        # Join miette's wrapped diagnostic, keeping the specific missing feature.
        clean = " ".join(line.strip(" │×╰─") for line in text.splitlines() if line.strip())
        match = re.search(r"Unsupported SystemVerilog construct: (.*)", clean)
        evidence = match[1].rstrip(") ") if match else evidence
    elif text.startswith("elaboration requires a module root;") or text.startswith("multiple module roots ("):
        category, diagnostic = "unsupported", "module_root_requirement"
    elif text.startswith("SystemVerilog parse error:"):
        category, diagnostic = "compile_error", "parse_error"
    elif re.search(r"^illegal_context$", text, re.MULTILINE):
        category, diagnostic = "compile_error", "illegal_context"
    elif re.search(r"^.*include directory is unavailable:", text, re.MULTILINE):
        category, diagnostic = "environment", "missing_include_directory"
    elif re.search(r"^\$fatal: |^\d+ runtime events were lost$", text, re.MULTILINE):
        category, diagnostic = "runtime_error", "runtime_diagnostic"
        evidence = re.search(r"^\$fatal: .*|^\d+ runtime events were lost$", text, re.MULTILINE)[0]
    if row["result"] == "pass" and success == "1":
        diagnostic, evidence = "", ""
    if log and any(row.get(key) and metadata.get(key) != row[key] for key in ("rc", "mode", "tool_success")):
        category, diagnostic, evidence = "unclassified", "metadata_mismatch", "TSV and official log metadata disagree"
    # Driver failure in simulation mode can still be a compiler failure. Leave
    # unknown diagnostics unclassified instead of labelling them runtime bugs.
    return {
        "failure_category": category if row["result"] == "fail" else "",
        "diagnostic": diagnostic if row["result"] != "skip" else "",
        "evidence": evidence[:1000] if row["result"] != "skip" else "",
        "negative_test": "1" if negative else "0" if metadata.get("should_fail") == "0" else "",
    }


def annotate(row, log):
    return {**row, **classify(row, log)}


def breakdown(rows):
    failures = [row for row in rows if row["result"] == "fail"]
    counts = Counter(row.get("failure_category") or "unclassified" for row in failures)
    diagnostics = Counter((row.get("failure_category") or "unclassified", row.get("diagnostic") or "unknown")
                          for row in failures)
    reasons = Counter()
    examples = {}
    for row in failures:
        evidence = re.sub(r"\S+\.svh?:\d+(?::\d+)?:?", "<source>", row.get("evidence", ""))
        key = (row.get("failure_category") or "unclassified", row.get("diagnostic") or "unknown", evidence)
        reasons[key] += 1
        examples.setdefault(key, {"test": row.get("test", ""), "log": row.get("log", "")})
    return {
        "by_failure_reason": [{"category": key[0], "diagnostic": key[1], "evidence": key[2],
                               "count": count, "example": examples[key]}
                              for key, count in sorted(reasons.items(), key=lambda item: (-item[1], item[0]))],
        "by_failure_category": dict(sorted(counts.items())),
        "by_failure_diagnostic": [{"category": category, "diagnostic": diagnostic, "count": count}
                                  for (category, diagnostic), count in sorted(diagnostics.items())],
        "negative_pass_diagnostics": dict(sorted(Counter(
            row["diagnostic"] for row in rows
            if row["result"] == "pass" and row.get("negative_test") == "1"
            and row.get("diagnostic") in {"unsupported_construct", "module_root_requirement", "runner_timeout", "rust_panic", "abnormal_exit"}
        ).items())),
    }


def markdown(summary):
    def cell(value):
        return str(value).replace("|", "\\|").replace("\n", " ")
    lines = ["# sv-tests results", "", f"Corpus revision: `{summary.get('revision') or 'unknown'}`", ""]
    if summary["complete"] is None:
        lines += ["**Collection completeness is unknown: the source report has no completeness marker.**", ""]
    elif not summary["complete"]:
        lines += ["**INCOMPLETE: collection failed. These totals are not a full conformance result.**", ""]
    lines += ["| Group | State | Pass / executed | Pass % | Skipped |", "| --- | --- | ---: | ---: | ---: |"]
    for group, states in summary["groups"].items():
        for state, counts in states.items():
            lines.append(f"| {cell(group)} | {state} | {counts['passed']} / {counts['executed']} | {counts['pass_percent']} | {counts['skipped']} |")
    lines += ["", "## Failure categories", "", "| Group | State | Category | Count |", "| --- | --- | --- | ---: |"]
    for group, states in summary["groups"].items():
        for state, counts in states.items():
            for category, count in counts["by_failure_category"].items():
                lines.append(f"| {cell(group)} | {state} | {category} | {count} |")
    for group, states in summary["groups"].items():
        for state, counts in states.items():
            if counts["by_failure_reason"]:
                lines += ["", f"### Frequent diagnostics: {cell(group)} / {state}", "",
                          "| Count | Category | Diagnostic | Evidence |", "| ---: | --- | --- | --- |"]
                for reason in counts["by_failure_reason"][:10]:
                    lines.append(f"| {reason['count']} | {reason['category']} | {reason['diagnostic']} | {cell(reason['evidence'][:200])} |")
            if counts["negative_pass_diagnostics"]:
                lines += ["", f"Negative-test passes requiring diagnostic review ({cell(group)}, {state}): "
                          + cell(counts["negative_pass_diagnostics"]), ""]
    if summary["collection_errors"]:
        lines += ["", "## Collection errors", ""]
        lines += [f"- {cell(error.get('test', error['stage']))} {cell(error.get('state_mode', ''))}: {cell(error['message'])}"
                  for error in summary["collection_errors"]]
    lines += ["", "Official verdicts and denominators are unchanged. Categories describe observed evidence, not proven root causes.",
              "Unrecognized diagnostics remain unclassified. Case names, diagnostics and log paths are in the companion .failures.json.", ""]
    return "\n".join(lines)


def write_reports(path, rows, groups, revision, errors=(), requested=None, complete=True, meta_tags_expanded=True):
    path.parent.mkdir(parents=True, exist_ok=True)
    fieldnames = list(BASE_FIELDS) + list(FIELDS)
    with path.open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=fieldnames, delimiter="\t", lineterminator="\n", extrasaction="ignore")
        writer.writeheader()
        writer.writerows(rows)
    summary = {"schema_version": 2, "classification_version": CLASSIFICATION_VERSION,
               "revision": revision, "complete": False if errors else complete, "collected": len(rows),
               "meta_tags_expanded": meta_tags_expanded,
               "requested": requested, "collection_errors": list(errors), "groups": groups}
    path.with_suffix(".summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    path.with_suffix(".summary.md").write_text(markdown(summary))
    failures = [{key: row.get(key, "") for key in (*BASE_FIELDS, *FIELDS)}
                for row in rows if row["result"] == "fail"]
    path.with_suffix(".failures.json").write_text(json.dumps({
        "classification_version": CLASSIFICATION_VERSION, "complete": summary["complete"],
        "failures": failures, "collection_errors": list(errors),
    }, indent=2) + "\n")
    return summary


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path, help="existing run.py TSV (never expected.tsv)")
    parser.add_argument("--output", type=Path, required=True, help="new annotated TSV and companion reports")
    parser.add_argument("--log-root", type=Path, help="relocated logs: ROOT/STATE/TEST/official.log")
    parser.add_argument("--meta-tags", type=Path, help="pinned upstream conf/meta-tags.conf")
    args = parser.parse_args(argv)
    try:
        # Import here to keep the classifier reusable by run.py.
        from run import read_meta_tags, summarize
        if args.output.resolve() == args.report.resolve():
            raise ValueError("output must differ from the source report")
        with args.report.open(newline="") as stream:
            reader = csv.DictReader(stream, delimiter="\t")
            if not set(BASE_FIELDS).issubset(reader.fieldnames or []):
                raise ValueError("expected a run.py report with case metadata and log paths")
            rows, seen = [], set()
            for row in reader:
                key = (row["test"], row["state_mode"])
                test = Path(row["test"])
                if (None in row or any(value is None for value in row.values()) or key in seen
                        or not row["test"] or test.is_absolute() or ".." in test.parts
                        or row["state_mode"] not in ("2state", "4state") or row["result"] not in ("pass", "fail", "skip")):
                    raise ValueError(f"invalid or duplicate report row: {key}")
                seen.add(key)
                log = args.log_root / row["state_mode"] / test / "official.log" if args.log_root else Path(row["log"])
                if not args.log_root and not log.is_absolute():
                    log = args.report.parent / log
                row["log"] = str(log)
                rows.append(annotate(row, log.read_text() if log.is_file() else ""))
        companion = args.report.with_suffix(".summary.json")
        source = json.loads(companion.read_text()) if companion.is_file() else {}
        rows.sort(key=lambda row: (row["test"], row["state_mode"]))
        groups = summarize(rows, read_meta_tags(args.meta_tags) if args.meta_tags else {})
        write_reports(args.output, rows, groups, source.get("revision"),
                      source.get("collection_errors", []), source.get("requested"),
                      complete=source.get("complete"), meta_tags_expanded=args.meta_tags is not None)
        return 0
    except (OSError, ValueError) as error:
        print(f"sv-tests reporting: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
