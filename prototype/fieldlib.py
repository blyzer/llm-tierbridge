"""Field getters, reference-value picking and placeholder detection shared by compare.py and merge.py."""
from __future__ import annotations

import json
from collections import Counter

from common import is_first_party

NEAR_TOL = 0.06   # relative difference still counted as "near" (1,000,000 vs 1,048,576 = 4.6%)
PLACEHOLDER_SHARE = 0.20

def g_ctx(o): return o.get("contextWindow")
def g_max(o): return o.get("maxTokens")
def g_cost(o):
    c = o.get("cost")
    if not c or c["status"] == "unknown" or c.get("input") is None:
        return None
    return (round(c["input"], 6), round(c.get("output") or 0, 6))
def g_reason(o): return o.get("reasoning")
def g_efforts(o):
    t = o.get("thinking")
    return tuple(t["efforts"]) if t and t.get("efforts") else None
def g_mods(o): return tuple(o["input"]) if o.get("input") else None


FIELDS = {"contextWindow": g_ctx, "maxTokens": g_max, "cost(input,output)": g_cost, "reasoning": g_reason,
          "efforts": g_efforts, "inputModalities": g_mods}


def pick(cands, getter, gw_id):
    """-> (value, basis, nCandidatesWithValue). basis: first-party | majority | single | ambiguous | none."""
    vals = [(getter(e["obs"]), e) for e in cands]
    vals = [(v, e) for v, e in vals if v is not None]
    if not vals:
        return None, "none", 0
    fp = [(v, e) for v, e in vals if is_first_party(e, gw_id)]
    pool, basis = (fp, "first-party") if fp else (vals, "majority")
    cnt = Counter(json.dumps(v) for v, _ in pool)
    top, n = cnt.most_common(1)[0]
    if len(cnt) == 1:
        return json.loads(top), ("first-party" if fp else ("single" if len(pool) == 1 else "majority")), len(vals)
    if fp:   # first-party entries disagree among themselves: take the most common only if strict majority
        if n > len(pool) / 2:
            return json.loads(top), "first-party", len(vals)
        return None, "ambiguous", len(vals)
    if n > len(pool) / 2:
        return json.loads(top), "majority", len(vals)
    return None, "ambiguous", len(vals)


def same(field, a, b):
    if field.startswith("cost"):
        return all(abs(x - y) <= 1e-6 * max(1.0, abs(y)) for x, y in zip(a, b))
    if field in ("efforts", "inputModalities"):
        return set(a) == set(b)
    return a == b




# ---- placeholder detection ---------------------------------------------------------------
# A value is a PLACEHOLDER when one (ctx,max) pair / effort ladder covers >= PLACEHOLDER_SHARE of a source's rows:
# no real catalog has >=20% of its models with identical limits. Validated in compare.py
# (results["placeholderValidation"]) by comparing agreement with the references for placeholder rows vs the rest.
def detect_placeholders(rows):
    pairs = Counter((o.get("contextWindow"), o.get("maxTokens")) for o in rows)
    effs = Counter(g_efforts(o) for o in rows)
    return {
        "pairs": {k for k, v in pairs.items() if v / len(rows) >= PLACEHOLDER_SHARE and k != (None, None)},
        "efforts": {k for k, v in effs.items() if v / len(rows) >= PLACEHOLDER_SHARE and k is not None}}


def is_placeholder(ph, o, field):
    if field in ("contextWindow", "maxTokens", "maxInputTokens"):
        return (o.get("contextWindow"), o.get("maxTokens")) in ph["pairs"]
    if field == "efforts":
        return g_efforts(o) in ph["efforts"]
    return False
