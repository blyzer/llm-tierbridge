"""Validate every record in out/*.jsonl against schema/gateway-model.schema.json (Draft 2020-12)."""
import glob
import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".pylib"))
from jsonschema import Draft202012Validator  # noqa: E402

from common import OUT, ROOT  # noqa: E402

schema = json.load(open(f"{ROOT}/../schema/gateway-model.schema.json"))
Draft202012Validator.check_schema(schema)
v = Draft202012Validator(schema)
total = bad = 0
for path in sorted(glob.glob(f"{OUT}/obs.*.jsonl") + glob.glob(f"{OUT}/merged.*.jsonl")):
    n = b = 0
    first = None
    for line in open(path):
        n += 1
        rec = json.loads(line)
        errs = list(v.iter_errors(rec))
        if errs:
            b += 1
            first = first or (rec.get("id"), errs[0].message[:200], list(errs[0].absolute_path))
    total += n
    bad += b
    print(f"{os.path.basename(path):40s} {n:6d} records  {b:5d} invalid  {first or ''}")
print(f"TOTAL {total} records, {bad} invalid")
sys.exit(1 if bad else 0)
