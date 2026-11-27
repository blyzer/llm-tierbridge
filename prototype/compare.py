"""Match gateway models to reference catalogs and count field disagreements.

    python3 compare.py     # writes out/results.json (+ prints a summary)

Gateways : cliproxyapi-cache, omp-live:easycliproxy, omp-live:dsh
References: models.dev, modelparams.dev   (+ omp-bundled as a circularity probe only)
Matching rule: common.RefIndex (tiers T1..T4); reference value choice: first-party entry, else majority.
"""
from __future__ import annotations

import difflib
import json
import random
import re
from collections import Counter, defaultdict

from common import OUT, RefIndex, is_first_party, read_jsonl

# Heuristic ONLY (documented as such): ids that look like image/video/stt/tts/embedding/rerank products.
NONCHAT_RE = re.compile(r"(aihorde/|embed|bge-|e5-|voyage|rerank|whisper|asr|stt|tts|scribe|diffusion|stable[-_]|"
                        r"image|veo|wan-?\d|seedance|sora|flux|video|speech|transcribe|ocr|mpnet|minilm|sdxl|inpaint)", re.I)
GATEWAYS = ["cliproxyapi-cache", "omp-live:easycliproxy", "omp-live:dsh"]
random.seed(20261126)


def load(sid):
    return read_jsonl(f"{OUT}/obs.{sid.replace(':', '_')}.jsonl")


def entry_of(o):
    return {"provider": o["provider"], "id": o["id"], "canonicalId": o.get("canonicalId"), "obs": o}


gw = {s: load(s) for s in GATEWAYS}
ref_obs = {"models.dev": load("models.dev"), "modelparams.dev": load("modelparams.dev"),
           "omp-bundled": load("omp-bundled")}
index = {k: RefIndex([entry_of(o) for o in v]) for k, v in ref_obs.items()}


from fieldlib import (FIELDS, NEAR_TOL, PLACEHOLDER_SHARE, detect_placeholders, g_cost, g_ctx, g_efforts,  # noqa: E402
                      g_max, g_mods, g_reason, pick, same)


# ---- 1. matching --------------------------------------------------------------------------
results = {"counts": {}, "match": {}, "unmatched": {}, "fields": {}, "lifecycle": {}, "probe": {}}
match_cache = {}
for s, rows in gw.items():
    results["counts"][s] = len(rows)
    for rname in ("models.dev", "modelparams.dev", "omp-bundled"):
        for o in rows:
            match_cache[(s, rname, o["id"])] = index[rname].match(o["id"])

for s, rows in gw.items():
    m = {}
    for rname in ("models.dev", "modelparams.dev", "omp-bundled"):
        tiers = Counter(match_cache[(s, rname, o["id"])][0] or "unmatched" for o in rows)
        m[rname] = dict(tiers)
    either = sum(1 for o in rows if match_cache[(s, "models.dev", o["id"])][0] or match_cache[(s, "modelparams.dev", o["id"])][0])
    both = sum(1 for o in rows if match_cache[(s, "models.dev", o["id"])][0] and match_cache[(s, "modelparams.dev", o["id"])][0])
    m["either"] = either
    m["both"] = both
    m["neither"] = len(rows) - either
    m["models.dev_or_modelparams_exact_only(T1+T2)"] = sum(
        1 for o in rows if any((match_cache[(s, r, o["id"])][0] or "") in ("T1-exact-full", "T2-exact-leaf")
                               for r in ("models.dev", "modelparams.dev")))
    results["match"][s] = m

    un = [o for o in rows if not match_cache[(s, "models.dev", o["id"])][0] and not match_cache[(s, "modelparams.dev", o["id"])][0]]
    pre = Counter((o["id"].split("/")[0] if "/" in o["id"] else "(no prefix)") for o in un)
    smp = random.sample(un, min(15, len(un)))
    leafs = sorted({e["id"].split("/")[-1].lower() for e in ref_obs["models.dev"]})
    near = []
    for o in random.sample(un, min(40, len(un))):
        leaf = o["id"].split("/")[-1].lower()
        c = difflib.get_close_matches(leaf, leafs, n=1, cutoff=0.88)
        if c:
            near.append((o["id"], c[0]))
    nonchat = [o for o in un if NONCHAT_RE.search(o["id"])]
    results["unmatched"][s] = {"n": len(un), "nonChatLooking": len(nonchat), "chatLooking": len(un) - len(nonchat),
                               "chatLookingSample": [o["id"] for o in random.sample([o for o in un if o not in nonchat], min(25, len(un) - len(nonchat)))],
                               "topPrefixes": pre.most_common(12), "sample": [o["id"] for o in smp],
                               "nearMissInSample40": near}

# precision audit sample (matches by tier), for eyeballing
audit = {}
for tier in ("T1-exact-full", "T2-exact-leaf", "T3-normalized-leaf", "T4-date-stripped"):
    pairs = []
    for s, rows in gw.items():
        for o in rows:
            t, c = match_cache[(s, "models.dev", o["id"])]
            if t == tier:
                pairs.append((s, o["id"], c[0]["provider"] + ":" + c[0]["id"], len(c)))
    results["match"].setdefault("_audit_pool_size", {})[tier] = len(pairs)
    audit[tier] = random.sample(pairs, min(25, len(pairs)))
results["audit"] = audit

# ---- 1b. placeholder detection (fieldlib.detect_placeholders) --------------------------------
placeholder = {s: detect_placeholders(rows) for s, rows in gw.items()}
results["placeholders"] = {s: {"ctxMaxPairs": [list(p_) for p_ in v["pairs"]], "efforts": [list(e) for e in v["efforts"]]}
                           for s, v in placeholder.items()}


def is_placeholder(s, o, field):
    from fieldlib import is_placeholder as _ip
    return _ip(placeholder[s], o, field)


# ---- 2. field disagreements -------------------------------------------------------------
examples = defaultdict(list)
for s, rows in gw.items():
    for rname in ("models.dev", "modelparams.dev"):
        for field, getter in FIELDS.items():
            if rname == "modelparams.dev" and field not in ("maxTokens", "reasoning", "efforts"):
                continue
            st = Counter()
            direction = Counter()
            basis_ct = Counter()
            for o in rows:
                tier, cands = match_cache[(s, rname, o["id"])]
                if not tier:
                    st["unmatched"] += 1
                    continue
                gv = getter(o)
                rv, basis, _ = pick(cands, getter, o["id"])
                basis_ct[basis] += 1
                if gv is not None and is_placeholder(s, o, field):
                    st["gw_placeholder"] += 1
                    # validation: does the placeholder agree with the reference (it should rarely)?
                    if rv is not None and basis != "ambiguous":
                        results.setdefault("placeholderValidation", {}).setdefault(f"{s} vs {rname} :: {field}", Counter())[
                            "placeholder_agree" if same(field, gv, rv) else "placeholder_disagree"] += 1
                    continue
                if gv is not None and rv is not None and basis != "ambiguous":
                    results.setdefault("placeholderValidation", {}).setdefault(f"{s} vs {rname} :: {field}", Counter())[
                        "real_agree" if same(field, gv, rv) else "real_disagree"] += 1
                if basis == "ambiguous":
                    st["ref_ambiguous"] += 1
                    continue
                if rv is None and gv is None:
                    st["both_missing"] += 1
                elif gv is None:
                    st["gw_missing_ref_has"] += 1
                elif rv is None:
                    st["ref_missing_gw_has"] += 1
                else:
                    if field == "reasoning" and rname == "modelparams.dev" and rv is not True:
                        continue
                    if same(field, gv, rv):
                        st["agree"] += 1
                    elif field in ("contextWindow", "maxTokens") and abs(gv - rv) / max(gv, rv) <= NEAR_TOL:
                        st["near"] += 1          # unit/rounding noise: 1,000,000 vs 1,048,576 (4.6%), 202,800 vs 202,752
                    else:
                        st["disagree"] += 1
                        if field in ("contextWindow", "maxTokens"):
                            direction["gw<ref" if gv < rv else "gw>ref"] += 1
                        if field == "reasoning":
                            direction["gw=true,ref=false" if gv else "gw=false,ref=true"] += 1
                            if gv and rname == "models.dev":
                                nontext = any(e["obs"].get("output") not in (["text"], None) for e in cands)
                                direction["  of which ref output not text-only"] += nontext
                                direction["  of which id looks non-chat (heuristic)"] += bool(NONCHAT_RE.search(o["id"]))
                                direction["  of which either"] += bool(nontext or NONCHAT_RE.search(o["id"]))
                        if field in ("efforts", "inputModalities"):
                            a_, b_ = set(gv), set(rv)
                            direction["gw⊂ref" if a_ < b_ else "gw⊃ref" if a_ > b_ else "differ"] += 1
                        examples[(s, rname, field)].append(
                            {"gwId": o["id"], "gw": gv, "ref": rv, "refEntry": cands[0]["provider"] + ":" + cands[0]["id"],
                             "tier": tier, "basis": basis})
            results["fields"][f"{s} vs {rname} :: {field}"] = {**st, "direction": dict(direction),
                                                                 "refBasis": dict(basis_ct)}

# ---- 3. lifecycle: gateway models whose reference says deprecated/retired ----------------------
for s, rows in gw.items():
    lc = Counter()
    ex = []
    for o in rows:
        for rname in ("models.dev", "modelparams.dev"):
            tier, cands = match_cache[(s, rname, o["id"])]
            if not tier:
                continue
            fps = [e for e in cands if is_first_party(e, o["id"])] or cands
            stats = Counter((e["obs"].get("lifecycle") or {}).get("status", "unknown") for e in fps)
            if stats and set(stats) <= {"retired", "deprecated"}:
                lc[f"{rname}:all-ref-entries-{'/'.join(sorted(stats))}"] += 1
                if len(ex) < 10:
                    ex.append((o["id"], rname, dict(stats), fps[0]["obs"]["lifecycle"]))
    results["lifecycle"][s] = {"counts": dict(lc), "examples": ex}

# ---- 4. probe: are the two views of the same gateway independent? -----------------------------
A = {o["id"]: o for o in gw["cliproxyapi-cache"]}
B = {o["id"]: o for o in gw["omp-live:easycliproxy"]}
both_ids = set(A) & set(B)
pr = {"cache_n": len(A), "live_n": len(B), "both": len(both_ids), "cache_only": sorted(set(A) - set(B))[:10],
      "cache_only_n": len(set(A) - set(B)), "live_only": sorted(set(B) - set(A))[:10], "live_only_n": len(set(B) - set(A))}
cmpA = {}
for field, getter in FIELDS.items():
    st = Counter()
    for i in both_ids:
        a, b = getter(A[i]), getter(B[i])
        if a is None or b is None:
            st["one_missing"] += 1
        elif same(field, a, b):
            st["agree"] += 1
        else:
            st["disagree"] += 1
            if len(examples[("cache-vs-live", "-", field)]) < 200:
                examples[("cache-vs-live", "-", field)].append({"gwId": i, "gw": a, "ref": b, "refEntry": "live", "tier": "id", "basis": "-"})
    cmpA[field] = dict(st)
pr["cache_vs_live_same_id"] = cmpA
# who does each view agree with when it disagrees with the other? (circularity probe)
for field in ("contextWindow", "maxTokens"):
    getter = FIELDS[field]
    cnt = Counter()
    for i in both_ids:
        a, b = getter(A[i]), getter(B[i])
        if a is None or b is None or a == b:
            continue
        t, cands = index["models.dev"].match(i)
        rv, basis, _ = pick(cands, getter, i) if t else (None, "none", 0)
        tb, cb = index["omp-bundled"].match(i)
        bv, bbasis, _ = pick(cb, getter, i) if tb else (None, "none", 0)
        cnt["live==models.dev"] += (b == rv)
        cnt["cache==models.dev"] += (a == rv)
        cnt["live==omp-bundled"] += (b == bv)
        cnt["cache==omp-bundled"] += (a == bv)
        cnt["n_disagreeing"] += 1
    pr[f"disagree_alignment::{field}"] = dict(cnt)
# placeholder/default detection
for s in ("cliproxyapi-cache", "omp-live:easycliproxy", "omp-live:dsh"):
    pairs = Counter((o.get("contextWindow"), o.get("maxTokens")) for o in gw[s])
    pr[f"top_(ctx,max)_pairs::{s}"] = [(list(k), v, round(100 * v / len(gw[s]), 1)) for k, v in pairs.most_common(5)]
    ct = Counter((o.get("cost") or {}).get("status") for o in gw[s])
    pr[f"cost_status::{s}"] = dict(ct)
    pr[f"thinking_mode::{s}"] = dict(Counter((o.get("thinking") or {}).get("mode") for o in gw[s]))
    pr[f"efforts_top::{s}"] = [(list(k) if k else None, v) for k, v in Counter(g_efforts(o) for o in gw[s]).most_common(5)]
results["probe"] = pr

# ---- 5. union over the three gateways (unique wire ids) ------------------------------------
uni = {}
for s, rows in gw.items():
    for o in rows:
        uni.setdefault(o["id"], []).append(s)
u_md = sum(1 for i in uni if index["models.dev"].match(i)[0])
u_mp = sum(1 for i in uni if index["modelparams.dev"].match(i)[0])
u_either = sum(1 for i in uni if index["models.dev"].match(i)[0] or index["modelparams.dev"].match(i)[0])
results["union"] = {"uniqueIds": len(uni), "models.dev": u_md, "modelparams.dev": u_mp, "either": u_either,
                    "neither": len(uni) - u_either, "inMoreThanOneSource": sum(1 for v in uni.values() if len(v) > 1)}

# ---- write ---------------------------------------------------------------------------------
for k, v in results.get("placeholderValidation", {}).items():
    results["placeholderValidation"][k] = dict(v)
json.dump(results, open(f"{OUT}/results.json", "w"), indent=1, default=list)
ex_out = {}
for k, v in examples.items():
    ex_out["||".join(k)] = {"n": len(v), "sample": random.sample(v, min(10, len(v)))}
json.dump(ex_out, open(f"{OUT}/examples.json", "w"), indent=1, default=list)

for s in GATEWAYS:
    print(s, results["counts"][s], json.dumps(results["match"][s]))
for k, v in results["fields"].items():
    print(k, json.dumps(v))
