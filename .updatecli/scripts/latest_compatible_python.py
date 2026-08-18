#!/usr/bin/env python3
"""Select the newest CPython minor supported by a cocotb release."""

from __future__ import annotations

import json
import re
import sys
from typing import Any
from urllib.request import Request, urlopen

PYPI_RELEASE_URL = "https://pypi.org/pypi/cocotb/{version}/json"
PYTHON_VERSIONS_MANIFEST_URL = (
    "https://raw.githubusercontent.com/actions/python-versions/"
    "main/versions-manifest.json"
)
CPYTHON_WHEEL_RE = re.compile(r"-cp(?P<abi>[0-9]+)-cp(?P=abi)-")
PYTHON_VERSION_RE = re.compile(r"^(?P<major>[0-9]+)\.(?P<minor>[0-9]+)\.[0-9]+$")


def load_json(url: str) -> Any:
    request = Request(url, headers={"User-Agent": "celox-updatecli"})
    with urlopen(request, timeout=30) as response:
        return json.load(response)


def cocotb_linux_x64_minors(release: dict[str, Any]) -> set[tuple[int, int]]:
    minors: set[tuple[int, int]] = set()

    for artifact in release.get("urls", []):
        if artifact.get("packagetype") != "bdist_wheel":
            continue

        filename = artifact.get("filename", "")
        if "manylinux" not in filename or "x86_64" not in filename:
            continue

        match = CPYTHON_WHEEL_RE.search(filename)
        if match is None:
            continue

        abi = match.group("abi")
        if len(abi) < 2:
            continue

        minors.add((int(abi[0]), int(abi[1:])))

    return minors


def actions_linux_x64_minors(manifest: list[dict[str, Any]]) -> set[tuple[int, int]]:
    minors: set[tuple[int, int]] = set()

    for release in manifest:
        if not release.get("stable", False):
            continue

        match = PYTHON_VERSION_RE.fullmatch(release.get("version", ""))
        if match is None:
            continue

        has_linux_x64 = any(
            artifact.get("platform") == "linux" and artifact.get("arch") == "x64"
            for artifact in release.get("files", [])
        )
        if has_linux_x64:
            minors.add((int(match.group("major")), int(match.group("minor"))))

    return minors


def latest_compatible_minor(
    cocotb_release: dict[str, Any], python_manifest: list[dict[str, Any]]
) -> tuple[int, int]:
    compatible = cocotb_linux_x64_minors(cocotb_release) & actions_linux_x64_minors(
        python_manifest
    )
    if not compatible:
        raise RuntimeError("no compatible stable CPython Linux x64 release found")

    return max(compatible)


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <cocotb-version>", file=sys.stderr)
        return 2

    cocotb_version = sys.argv[1]
    cocotb_release = load_json(PYPI_RELEASE_URL.format(version=cocotb_version))
    python_manifest = load_json(PYTHON_VERSIONS_MANIFEST_URL)
    major, minor = latest_compatible_minor(cocotb_release, python_manifest)
    print(f"{major}.{minor}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
