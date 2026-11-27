#!/usr/bin/env python3
"""Computes the set of PRIVATE model ids and checks that none of them is in the tracked tree.

Private ids = the ids of the rosters kept out of the public tree (`local-fixtures/`: the CLIProxyAPI
cache, the `omp models <provider>` listings and the router-for-me/models snapshot) MINUS everything
that appears in a public source we keep (models.dev, modelparams.dev, the dsh registry, freellmapi,
omp-bundled; `crates/catalog/tests/fixtures/`). Three sets are derived and matched against every
tracked text file (exact tokens, never substrings):

  full    a private id that is not a string of any public source;
  leaf    the part after the last `/` of a private id, when no public source has that leaf
          (matched only if it looks like an id: it has a digit or one of `-_.:`);
  prefix  the first segment of a private `a/b` id, when no public source uses it.

A token matches when it, or the token without a `tb-` alias prefix, is in `full`; when its leaf is
in `leaf` (and id-like); or when it is `a/...` with `a` in `prefix`.

It prints COUNTS ONLY: the ids never go to stdout, so the output is safe to paste or commit.
`--hits` adds `file:line` positions (still no ids). Exit status 1 when anything matches.

    python3 tools/private-ids.py [--hits] [--fixtures DIR] [--local DIR]

`--local` defaults to $TIERBRIDGE_LOCAL_FIXTURES or `<repo>/local-fixtures`.
"""

import argparse
import collections
import io
import json
import os
import re
import subprocess
import sys

try:  # Python 3.14+
    from compression import zstd as _zstd
except ImportError:  # fall back to the zstd CLI
    _zstd = None

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TOKEN = re.compile(r"[A-Za-z0-9_.:+@-]+(?:/[A-Za-z0-9_.:+@-]+)*")
ALIAS_PREFIX = "tb-"
IDLIKE = re.compile(r"[-_.:0-9]")  # a bare word ("smart") is prose, not evidence of an id

PUBLIC = [  # (file prefix, kind) in crates/catalog/tests/fixtures/
    "models.dev.api.",
    "modelparams.models.",
    "dsh.",
    "freellmapi.",
    "omp-oracle-models.",
]
PRIVATE = [
    "cliproxyapi-models.",
    "omp-models-easycliproxy.",
    "omp-models-dsh.",
    "router-for-me.models.",
]


def load(path):
    if _zstd is not None:
        with open(path, "rb") as f:
            raw = _zstd.decompress(f.read())
    else:
        raw = subprocess.run(["zstd", "-dc", path], check=True, capture_output=True).stdout
    return json.loads(raw)


def strings(node, out):
    """Every string (values and keys) of a JSON document."""
    if isinstance(node, str):
        out.add(node)
    elif isinstance(node, list):
        for v in node:
            strings(v, out)
    elif isinstance(node, dict):
        for k, v in node.items():
            out.add(k)
            strings(v, out)


def leaf(s):
    return s.rsplit("/", 1)[-1]


def roster_ids(doc):
    """The model ids of one private roster, whatever its envelope."""
    ids = set()

    def walk(node):
        if isinstance(node, dict):
            if isinstance(node.get("id"), str):
                ids.add(node["id"])
            for v in node.values():
                walk(v)
        elif isinstance(node, list):
            for v in node:
                walk(v)

    walk(doc)
    return ids


def find(directory, prefix):
    names = sorted(n for n in os.listdir(directory) if n.startswith(prefix) and n.endswith(".zst"))
    return [os.path.join(directory, n) for n in names if not n.startswith("obs.")]


def compute(fixtures, local):
    public = set()
    for prefix in PUBLIC:
        for path in find(fixtures, prefix):
            strings(load(path), public)
    private, per_roster = set(), {}
    for prefix in PRIVATE:
        for path in find(local, prefix):
            ids = roster_ids(load(path))
            per_roster[os.path.basename(path).split(".")[0]] = len(ids)
            private |= ids
    if not private:
        sys.exit(f"no private roster found in {local} (see README, 'Pruebas')")
    public_leaves = {leaf(s) for s in public}
    public_prefixes = {s.split("/", 1)[0] for s in public} | public
    full = {i for i in private if i not in public}
    leaves = {leaf(i) for i in private} - public_leaves
    prefixes = {i.split("/", 1)[0] for i in private if "/" in i} - public_prefixes
    return full, leaves, prefixes, per_roster, len(private)


def matches(token, full, leaves, prefixes):
    token = token.strip(".:-+")
    candidates = {token}
    if token.startswith(ALIAS_PREFIX):
        candidates.add(token[len(ALIAS_PREFIX):])
    for c in candidates:
        if c in full or c.lower() in full:
            return True
        if leaf(c) in leaves and IDLIKE.search(leaf(c)):
            return True
        if "/" in c and c.split("/", 1)[0] in prefixes:
            return True
    return False


def tracked_text_files():
    out = subprocess.run(
        ["git", "ls-files", "-z"], cwd=ROOT, check=True, capture_output=True
    ).stdout.decode()
    for name in filter(None, out.split("\0")):
        if name.endswith(".zst") or name == "tools/private-ids.py":
            continue
        path = os.path.join(ROOT, name)
        if not os.path.isfile(path):
            continue
        try:
            with io.open(path, encoding="utf-8") as f:
                yield name, f.read()
        except UnicodeDecodeError:
            continue


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--hits", action="store_true", help="print file:line positions (no ids)")
    ap.add_argument("--fixtures", default=os.path.join(ROOT, "crates/catalog/tests/fixtures"))
    ap.add_argument(
        "--local",
        default=os.environ.get("TIERBRIDGE_LOCAL_FIXTURES") or os.path.join(ROOT, "local-fixtures"),
    )
    args = ap.parse_args()

    full, leaves, prefixes, per_roster, n_private = compute(args.fixtures, args.local)
    print(f"rosters: {per_roster}")
    print(f"private ids: {n_private}; full: {len(full)}, leaf: {len(leaves)}, prefix: {len(prefixes)}")

    per_file = collections.Counter()
    for name, text in tracked_text_files():
        for lineno, line in enumerate(text.split("\n"), 1):
            n = sum(1 for m in TOKEN.finditer(line) if matches(m.group(0), full, leaves, prefixes))
            if n:
                per_file[name] += n
                if args.hits:
                    print(f"  {name}:{lineno}: {n}")
    for name, n in per_file.most_common():
        print(f"{n:5d}  {name}")
    print(f"hits: {sum(per_file.values())} in {len(per_file)} files")
    return 1 if per_file else 0


if __name__ == "__main__":
    sys.exit(main())
