#!/usr/bin/env python3
"""Check checkout-local Markdown links/fragments; never fetch external URLs."""
from pathlib import Path
import html
import os
import re
import subprocess
import sys
import unicodedata
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]
LINK = re.compile(r'!?\[[^\]\n]*\]\(<?([^\s)>]+)>?(?:\s+"[^"]*")?\)')


def prose(text):
    return re.sub(r'^\s*(`{3,}|~{3,})[^\n]*\n.*?^\s*\1\s*$', '', text,
                  flags=re.M | re.S)


def anchors(text):
    seen = {}
    result = set()
    for heading in re.findall(r'^#{1,6}\s+(.+?)\s*#*\s*$', prose(text), re.M):
        heading = re.sub(r'\[([^]]+)\]\([^)]*\)', r'\1', heading)
        heading = re.sub(r'<[^>]+>', '', heading)
        slug = ''.join(c for c in html.unescape(heading).lower()
                       if unicodedata.category(c)[0] in 'LN' or c in ' _-').replace(' ', '-')
        count = seen.get(slug, 0)
        seen[slug] = count + 1
        result.add(slug + (f'-{count}' if count else ''))
    result.update(re.findall(r'<a\s+(?:name|id)=["\x27]([^"\x27]+)', text))
    return result


def check():
    paths = subprocess.check_output(
        ['git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z', '--', '*.md'],
        cwd=ROOT).decode().split('\0')
    docs = sorted({p for p in paths if p and (ROOT / p).is_file()})
    errors = []
    checked = 0
    for name in docs:
        path = ROOT / name
        body = prose(path.read_text())
        # Inline Markdown links/images and explicit reference-style definitions.
        targets = LINK.findall(body) + re.findall(r'^\s*\[[^]]+\]:\s*<?([^\s>]+)', body, re.M)
        for target in targets:
            parsed = urlsplit(target)
            if parsed.scheme or parsed.netloc:
                continue
            local = (path.parent / unquote(parsed.path)).resolve() if parsed.path else path
            checked += 1
            if not local.is_relative_to(ROOT) or not local.exists():
                errors.append(f'{name}: missing/outside-checkout link {target}')
            elif parsed.fragment and local.suffix.lower() == '.md':
                if unquote(parsed.fragment) not in anchors(local.read_text()):
                    errors.append(f'{name}: missing heading {target}')
    for error in errors:
        print(error, file=sys.stderr)
    print(f'{len(docs)} Markdown files; {checked} local links checked; {len(errors)} errors')
    return bool(errors)


if __name__ == '__main__':
    raise SystemExit(check())
