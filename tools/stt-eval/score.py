# -*- coding: utf-8 -*-
"""给 whisper_bench 的输出补 CER / 关键词命中,打印汇总。用法:python score.py <bench.json> <sentences.tsv> <label>"""
import sys, json
from cer import cer, contains_all
KEYS = ["小七", "李四", "Alice", "王明", "张伟", "陈晨", "流式", "熔断", "受限级"]
rows = json.load(open(sys.argv[1], encoding="utf-8"))
refs = dict(l.rstrip("\n").split("\t", 1) for l in open(sys.argv[2], encoding="utf-8") if l.strip())
hits = tot = 0
for r in rows:
    ref = refs[r["file"].split("_")[0]]; r["ref"] = ref; r["cer"] = round(cer(ref, r["hyp"]), 3)
    k = contains_all(r["hyp"], [x for x in KEYS if x in ref]); r["keys"] = k; hits += sum(k.values()); tot += len(k)
summary = {"model": sys.argv[3], "n": len(rows), "rtf_mean": round(sum(r["rtf"] for r in rows) / len(rows), 3),
           "cer_mean": round(sum(r["cer"] for r in rows) / len(rows), 3), "key_hit_rate": round(hits / max(1, tot), 3)}
json.dump({"summary": summary, "rows": rows}, open(sys.argv[1], "w", encoding="utf-8"), ensure_ascii=False, indent=1)
print(json.dumps(summary, ensure_ascii=False))
