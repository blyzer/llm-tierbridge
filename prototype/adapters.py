"""Adapters: five raw inputs -> Observation JSONL conforming to schema/gateway-model.schema.json.

    python3 adapters.py            # writes out/obs.<sourceId>.jsonl and prints counts

Sources
  models.dev          raw/models.dev.api.json                 (reference; bundled snapshot)
  modelparams.dev     raw/modelparams.models.json             (reference; bundled snapshot)
  cliproxyapi-cache   ~/.omp/agent/cliproxyapi-models.json    (gateway; discovered)
  omp-live:easycliproxy / omp-live:dsh   raw/omp-models-<p>.json  (gateway; `omp models <p> --json`)
  omp-bundled         raw/omp-oracle-models.json              (probe only: omp's own checked-in roster)
"""
from __future__ import annotations

import hashlib
import json
import os

from common import (CLIPROXY_CACHE, FETCH_DATE, OUT, RAW, cost_obj, effort, efforts_sorted, iso_ms,
                    observation, source, write_jsonl)

FETCH_MS = iso_ms(FETCH_DATE)


def sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()[:16]


def posint(v):
    return int(v) if isinstance(v, (int, float)) and v > 0 else None


def nonneg(v):
    """models.dev uses -1 as 'dynamic budget' sentinel; never let a sentinel leak into a numeric field."""
    return v if isinstance(v, (int, float)) and v >= 0 else None


def mods(v):
    return sorted(set(v)) if v else None


# ---------------------------------------------------------------------------------------------
def adapt_models_dev():
    path = f"{RAW}/models.dev.api.json"
    data = json.load(open(path))
    src = source("models.dev", "bundled", f"https://models.dev/api.json#sha256={sha(path)}", "declared", FETCH_MS,
                 revision=FETCH_DATE)
    rows = []
    for pid, p in data.items():
        for mid, m in p["models"].items():
            lim = m.get("limit") or {}
            c = m.get("cost")
            tiers = []
            if c:
                for t in c.get("tiers") or []:
                    if (t.get("tier") or {}).get("type") == "context" and t["tier"].get("size"):
                        tiers.append({"promptTokensAbove": int(t["tier"]["size"]), "input": t.get("input"),
                                      "output": t.get("output"), "cacheRead": t.get("cache_read"),
                                      "cacheWrite": t.get("cache_write")})
                if not tiers and c.get("context_over_200k"):
                    t = c["context_over_200k"]
                    tiers.append({"promptTokensAbove": 200000, "input": t.get("input"), "output": t.get("output"),
                                  "cacheRead": t.get("cache_read"), "cacheWrite": t.get("cache_write")})
            cost = cost_obj(c.get("input"), c.get("output"), c.get("cache_read"), c.get("cache_write"), tiers,
                            zero_means_unknown=False) if c else None
            # reasoning_options -> Thinking
            thinking = None
            ro = m.get("reasoning_options") or []
            if not m.get("reasoning"):
                thinking = {"mode": "none", "efforts": []}
            elif ro:
                eff, budget, toggle = [], None, False
                for o in ro:
                    if o["type"] == "effort":
                        eff += o.get("values", [])
                    elif o["type"] == "budget_tokens":
                        budget = o
                    elif o["type"] == "toggle":
                        toggle = True
                ef = efforts_sorted(eff)
                thinking = {"mode": "effort" if ef else ("budget" if budget else "toggle"), "efforts": ef,
                            "canDisable": True if (toggle or "off" in ef) else None,
                            "minBudgetTokens": nonneg((budget or {}).get("min")),
                            "maxBudgetTokens": nonneg((budget or {}).get("max")),
                            "dynamicBudget": True if (budget or {}).get("min") == -1 else None}
            else:
                thinking = {"mode": "unknown", "efforts": []}
            st = m.get("status")
            life = {"status": {"deprecated": "deprecated", "beta": "preview", "alpha": "preview"}.get(st, "live"),
                    "replacement": None, "shutdownOn": None}
            rows.append(observation(
                src, pid, mid, name=m.get("name"), canonicalId=m.get("canonical_model_id"), kind=None,
                contextWindow=posint(lim.get("context")), maxInputTokens=posint(lim.get("input")),
                maxTokens=posint(lim.get("output")), cost=cost,
                input=mods((m.get("modalities") or {}).get("input")),
                output=mods((m.get("modalities") or {}).get("output")),
                reasoning=bool(m.get("reasoning")), thinking=thinking, supportsTools=m.get("tool_call"),
                lifecycle=life, acceptedParams=None, authType=None,
                raw={"releaseDate": m.get("release_date"), "family": m.get("family")}))
    return rows


# ---------------------------------------------------------------------------------------------
_LEN_PATHS = {"max_tokens", "max_completion_tokens", "max_output_tokens", "inferenceConfig.maxTokens",
              "generationConfig.maxOutputTokens"}


def adapt_modelparams():
    path = f"{RAW}/modelparams.models.json"
    data = json.load(open(path))
    gen_ms = iso_ms(data["generatedAt"])
    src = source("modelparams.dev", "bundled", f"https://modelparams.dev/api/v1/models.json#sha256={sha(path)}",
                 "declared", FETCH_MS, revision=data["generatedAt"])
    rows = []
    for e in data["models"]:
        params, eff, has_reasoning, max_out = [], [], False, None
        for p in e["params"]:
            params.append({"path": p["path"], "type": p["type"], "group": p.get("group"), "values": p.get("values"),
                           "range": p.get("range"), "default": p.get("default"),
                           "conditional": "applicability" in p})
            if p.get("group") == "reasoning":
                has_reasoning = True
            if p.get("group") == "reasoning" and p["type"] == "enum" and "effort" in p["path"].lower():
                eff += p.get("values") or []
            if p["path"] in _LEN_PATHS and (p.get("range") or {}).get("max"):
                max_out = int(p["range"]["max"])
        ef = efforts_sorted(eff)
        thinking = {"mode": "effort", "efforts": ef, "canDisable": True if "off" in ef else None} if ef else None
        st = e.get("status")
        life = {"status": {"active": "live", "deprecated": "deprecated", "retired": "retired"}.get(st, "unknown"),
                "replacement": e.get("replacement"), "shutdownOn": e.get("shutdownOn")}
        rows.append(observation(
            src, e["provider"], e["model"], name=None, canonicalId=None, kind=None, maxTokens=max_out,
            reasoning=True if has_reasoning else None,   # absence of reasoning params is NOT evidence of no reasoning
            thinking=thinking, lifecycle=life, acceptedParams=params, authType=e["authType"],
            raw={"apiSurface": e["apiSurface"], **({"wireId": e["wireId"]} if "wireId" in e else {})}))
    return rows


# ---------------------------------------------------------------------------------------------
def _cache_cost(c):
    tiers = [{"promptTokensAbove": int(t["inputTokensAbove"]), "input": t.get("input"), "output": t.get("output"),
              "cacheRead": t.get("cacheRead"), "cacheWrite": t.get("cacheWrite")}
             for t in (c.get("tiers") or []) if t.get("inputTokensAbove")]
    return cost_obj(c.get("input"), c.get("output"), c.get("cacheRead"), c.get("cacheWrite"), tiers,
                    zero_means_unknown=True)


def _level_map_thinking(m):
    """cliproxyapi/omp-bundled rows: thinkingLevelMap {level: native|null}."""
    if not m.get("reasoning"):
        return {"mode": "none", "efforts": []}
    lm = m.get("thinkingLevelMap")
    if lm is None:
        return {"mode": "unknown", "efforts": []}
    ef = efforts_sorted([k for k, v in lm.items() if v is not None])
    return {"mode": "effort" if ef else "unknown", "efforts": ef,
            "effortMap": {k: v for k, v in lm.items() if effort(k)}}


def adapt_cliproxy_cache():
    d = json.load(open(CLIPROXY_CACHE))
    src = source("cliproxyapi-cache", "discovered", d.get("modelsUrl") or "cliproxyapi-models.json", "inferred",
                 d["fetchedAt"], revision=f"fetchedAt={d['fetchedAt']}")
    rows = []
    for m in d["models"]:
        rows.append(observation(
            src, "cliproxyapi", m["id"], name=m.get("name"), kind="chat", contextWindow=posint(m.get("contextWindow")),
            maxTokens=posint(m.get("maxTokens")), cost=_cache_cost(m["cost"]), input=mods(m.get("input")),
            reasoning=m.get("reasoning"), thinking=_level_map_thinking(m),
            raw={"stale": m.get("stale"), "lastSeenAt": m.get("lastSeenAt")}))
    return rows


def adapt_omp_live(provider):
    path = f"{RAW}/omp-models-{provider}.json"
    d = json.load(open(path))
    src = source(f"omp-live:{provider}", "discovered", f"omp models {provider} --json --no-extensions", "inferred",
                 int(os.path.getmtime(path) * 1000), revision="omp 19.2.1")
    rows = []
    for m in d["models"]:
        th = m.get("thinking")
        if not m.get("reasoning"):
            thinking = {"mode": "none", "efforts": []}
        elif th:
            thinking = {"mode": "effort", "efforts": efforts_sorted(th)}
        else:
            thinking = {"mode": "unknown", "efforts": []}
        c = m["cost"]
        cost = cost_obj(c.get("input"), c.get("output"), c.get("cacheRead"), c.get("cacheWrite"), [],
                        zero_means_unknown=(m.get("pricingStatus") == "unknown"))
        rows.append(observation(
            src, provider, m["id"], selector=m.get("selector"), name=m.get("name"), kind=m.get("kind"),
            contextWindow=posint(m.get("contextWindow")), maxTokens=posint(m.get("maxTokens")), cost=cost,
            input=mods(m.get("input")), reasoning=m.get("reasoning"), thinking=thinking,
            raw={"pricingStatus": m.get("pricingStatus")}))
    return rows


def adapt_omp_bundled():
    path = f"{RAW}/omp-oracle-models.json"
    d = json.load(open(path))
    src = source("omp-bundled", "bundled", "fixtures/llm-oracle/catalog/models.json.zst", "declared",
                 int(os.path.getmtime(path) * 1000), revision="oh-my-pi working tree")
    rows = []
    for pid, ms in d.items():
        for mid, m in ms.items():
            rows.append(observation(
                src, pid, mid, name=m.get("name"), contextWindow=posint(m.get("contextWindow")),
                maxTokens=posint(m.get("maxTokens")), cost=cost_obj(
                    m["cost"].get("input"), m["cost"].get("output"), m["cost"].get("cacheRead"),
                    m["cost"].get("cacheWrite"), [], zero_means_unknown=False),
                input=mods(m.get("input")), reasoning=m.get("reasoning"), thinking=(
                    {"mode": "none", "efforts": []} if not m.get("reasoning") else
                    {"mode": "effort", "efforts": efforts_sorted(m["thinking"]["efforts"])} if m.get("thinking")
                    else {"mode": "unknown", "efforts": []})))
    return rows


ADAPTERS = {
    "models.dev": adapt_models_dev,
    "modelparams.dev": adapt_modelparams,
    "cliproxyapi-cache": adapt_cliproxy_cache,
    "omp-live:easycliproxy": lambda: adapt_omp_live("easycliproxy"),
    "omp-live:dsh": lambda: adapt_omp_live("dsh"),
    "omp-bundled": adapt_omp_bundled,
}

if __name__ == "__main__":
    for sid, fn in ADAPTERS.items():
        rows = fn()
        write_jsonl(f"{OUT}/obs.{sid.replace(':', '_')}.jsonl", rows)
        print(f"{sid:24s} {len(rows):6d} observations")
