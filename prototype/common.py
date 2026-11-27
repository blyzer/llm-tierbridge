"""Shared helpers: paths, source descriptors, Observation builder, id-matching rule.

Python 3 stdlib only (jsonschema, if present, is vendored in .pylib for validate.py).
"""
from __future__ import annotations

import datetime
import json
import os
import re
from collections import defaultdict

ROOT = os.path.dirname(os.path.abspath(__file__))
RAW = os.path.join(ROOT, "raw")
OUT = os.path.join(ROOT, "out")
os.makedirs(OUT, exist_ok=True)

HOME = os.path.expanduser("~")
CLIPROXY_CACHE = os.path.join(HOME, ".omp/agent/cliproxyapi-models.json")

FETCH_DATE = open(os.path.join(RAW, "fetch-date.txt")).read().strip()  # UTC ISO, written when raw was downloaded


def iso_ms(iso: str) -> int:
    return int(datetime.datetime.fromisoformat(iso.replace("Z", "+00:00")).timestamp() * 1000)


# ---- source descriptors (crate ProvenanceSource + sourceId) ----------------------------------
def source(source_id, kind, origin, confidence, observed_at_ms=None, revision=None):
    return {"sourceId": source_id, "kind": kind, "origin": origin, "revision": revision,
            "confidence": confidence, "observedAtMs": observed_at_ms}


EFFORT_ORDER = ["off", "minimal", "low", "medium", "high", "xhigh", "max"]
_EFFORT_ALIAS = {"none": "off", "x_high": "xhigh", "extra-high": "xhigh", "extra_high": "xhigh"}


def effort(v):
    """Map a source's effort spelling to crate ReasoningEffort; None if outside the enum
    (e.g. 'default', 'ultra')."""
    v = _EFFORT_ALIAS.get(str(v).lower(), str(v).lower())
    return v if v in EFFORT_ORDER else None


def efforts_sorted(vals):
    out = {effort(v) for v in vals} - {None}
    return [e for e in EFFORT_ORDER if e in out]


def cost_obj(inp, out, cr, cw, tiers, *, zero_means_unknown):
    """Build Cost. `zero_means_unknown` is True for gateway sources that fill 0 when they have no price."""
    # sentinel prices (omp-bundled openrouter/auto uses -1e6 for "variable pricing") are unknown, not negative
    inp, out, cr, cw = [None if (v is not None and v < 0) else v for v in (inp, out, cr, cw)]
    if any(t.get(k) is not None and t[k] < 0 for t in (tiers or []) for k in ("input", "output", "cacheRead", "cacheWrite")):
        tiers = []
    vals = [inp, out, cr, cw]
    if all(v is None for v in vals):
        return {"status": "unknown", "input": None, "output": None, "cacheRead": None, "cacheWrite": None}
    allzero = all((v or 0) == 0 for v in vals) and not tiers
    if allzero:
        if zero_means_unknown:
            return {"status": "unknown", "input": None, "output": None, "cacheRead": None, "cacheWrite": None}
        return {"status": "free", "input": 0, "output": 0, "cacheRead": cr, "cacheWrite": cw}
    c = {"status": "known", "input": inp, "output": out, "cacheRead": cr, "cacheWrite": cw}
    if tiers:
        c["tiers"] = tiers
    return c


def observation(src, provider, mid, **kw):
    rec = {"recordType": "observation", "source": src, "provider": provider, "id": mid}
    rec.update({k: v for k, v in kw.items()})
    return rec


def write_jsonl(path, rows):
    with open(path, "w") as f:
        for r in rows:
            f.write(json.dumps(r, separators=(",", ":"), ensure_ascii=False) + "\n")


def read_jsonl(path):
    with open(path) as f:
        return [json.loads(line) for line in f]


# ---- id-matching rule ------------------------------------------------------------------------
# Marker vocabulary copied from crates/catalog/compat/taxonomy/_discovery.kdl (`trailing-marker`,
# `billing-variant-suffix`, `wrapper-prefix`, `synthetic-prefix`). `-max`, `-none`, `-extra-high`
# are deliberately NOT stripped (the crate says they name real product lanes).
TRAILING_MARKERS = ["customtools", "cloud", "exacto", "nitro", "original", "optimized", "nvfp4", "fp8", "fp4",
                    "bf16", "int8", "int4", "thinking", "minimal", "low", "medium", "high", "xhigh", "free",
                    "contributor"]
WRAPPER_PREFIXES = ["duo-chat-"]
SYNTHETIC_PREFIXES = ["hf:"]
_MARK_RE = re.compile(r"[-:](?:%s)$" % "|".join(TRAILING_MARKERS))
_DATE_RE = re.compile(r"-(?:20\d{6}|20\d{2}-\d{2}-\d{2}|2[3-9](?:0[1-9]|1[0-2]))$")  # -20250929, -2025-09-29, -2507


def split_id(raw: str):
    """-> (prefix_segments, leaf). Lowercased; synthetic prefix and wrapper prefix stripped."""
    s = raw.strip().lower()
    for p in SYNTHETIC_PREFIXES:
        if s.startswith(p):
            s = s[len(p):]
    parts = s.split("/")
    leaf = parts[-1]
    for p in WRAPPER_PREFIXES:
        if leaf.startswith(p):
            leaf = leaf[len(p):]
    return parts[:-1], leaf


# crate `canonical-family-token` list: attached version separator (`gemini2.5` -> `gemini-2.5`)
FAMILY_TOKENS = ["claude", "gemini", "gpt", "grok", "glm", "qwen", "minimax", "kimi", "deepseek", "llama", "gemma",
                 "nova", "mistral", "ministral", "pixtral", "codestral", "devstral", "magistral", "ernie", "doubao",
                 "seed", "aion", "olmo", "molmo", "nemotron", "palmyra", "command", "codex", "coder"]
_ATTACH_RE = re.compile(r"^(%s)(?=\d)" % "|".join(FAMILY_TOKENS))


def norm_leaf(leaf: str, *, strip_date=False, markers=True) -> str:
    x = _ATTACH_RE.sub(lambda m: m.group(1) + "-", leaf)
    x = re.sub(r"(?<=\d)\.(?=\d)", "-", x)   # 4.5 -> 4-5
    x = x.replace("_", "-")
    x = re.sub(r":latest$", "", x)
    while True:                                  # strip trailing markers repeatedly
        y = _MARK_RE.sub("", x) if markers else x
        if strip_date:
            y = _DATE_RE.sub("", y)
        if y == x:
            break
        x = y
    return x


TIERS = ["T1-exact-full", "T2-exact-leaf", "T3-normalized-leaf", "T4-date-stripped"]


class RefIndex:
    """Index of reference entries. Entry = dict(provider, id, canonicalId, ...)."""

    def __init__(self, entries):
        self.entries = entries
        self.by = {t: defaultdict(list) for t in TIERS}
        for i, e in enumerate(entries):
            names = [e["id"]] + ([e["canonicalId"]] if e.get("canonicalId") else [])
            for n in names:
                low = n.lower()
                self.by["T1-exact-full"][low].append(i)
                _, leaf = split_id(low)
                self.by["T2-exact-leaf"][leaf].append(i)
                # Markers are stripped from the GATEWAY id only: a reference leaf like `vision-medium` is a real
                # product name, not an effort marker (audit found `gateway/vision` -> `vispark/vision-medium`).
                self.by["T3-normalized-leaf"][norm_leaf(leaf, markers=False)].append(i)
                self.by["T4-date-stripped"][norm_leaf(leaf, strip_date=True, markers=False)].append(i)
            self.by["T1-exact-full"][(e["provider"] + "/" + e["id"]).lower()].append(i)

    def match(self, gw_id: str):
        """Return (tier, [entries]) for the first tier that hits, else (None, [])."""
        low = gw_id.strip().lower()
        for hf in SYNTHETIC_PREFIXES:
            if low.startswith(hf):
                low = low[len(hf):]
        _, leaf = split_id(gw_id)
        keys = {"T1-exact-full": low, "T2-exact-leaf": leaf, "T3-normalized-leaf": norm_leaf(leaf),
                "T4-date-stripped": norm_leaf(leaf, strip_date=True)}
        for t in TIERS:
            hit = self.by[t].get(keys[t])
            if hit:
                seen, out = set(), []
                for i in hit:
                    if i not in seen:
                        seen.add(i)
                        out.append(self.entries[i])
                return t, out
        return None, []


def is_first_party(entry, gw_id: str) -> bool:
    """A reference entry is first-party when its provider is the vendor that owns the model:
    canonical_model_id's vendor segment == provider id, or (no canonical id) the vendor segment of the
    gateway id equals the provider id."""
    canon = entry.get("canonicalId")
    if canon and "/" in canon:
        return canon.split("/")[0].lower() == entry["provider"].lower()
    pre, _ = split_id(gw_id)
    return bool(pre) and pre[-1] == entry["provider"].lower()
