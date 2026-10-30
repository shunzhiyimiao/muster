# -*- coding: utf-8 -*-
"""字错率(CER):去掉标点空白、小写、繁转简(有 opencc 就转),再算编辑距离。"""
import re, sys, json
try:
    from opencc import OpenCC
    _t2s = OpenCC("t2s").convert
except Exception:
    _t2s = lambda s: s
_PUNCT = re.compile(r"[\s,,。.、;;::!!??\"'“”‘’()()《》〈〉\[\]【】—\-…~～·/]+")
def norm(s: str) -> str:
    return _PUNCT.sub("", _t2s(s)).lower()
def edit_distance(a: str, b: str) -> int:
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]
def cer(ref: str, hyp: str) -> float:
    r, h = norm(ref), norm(hyp)
    return edit_distance(r, h) / max(1, len(r))
def contains_all(hyp: str, keys) -> dict:
    h = norm(hyp)
    return {k: (norm(k) in h) for k in keys}
if __name__ == "__main__":
    ref, hyp = sys.argv[1], sys.argv[2]
    print(json.dumps({"cer": round(cer(ref, hyp), 3), "ref": norm(ref), "hyp": norm(hyp)}, ensure_ascii=False))
