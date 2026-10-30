# -*- coding: utf-8 -*-
"""DashScope Paraformer 实时识别(paraformer-realtime-v2,WebSocket 流式):按 100ms 实时送入,记录首个部分结果延迟、
句末到最终结果延迟、CER、关键词命中。需要环境变量 DASHSCOPE_API_KEY。
  python eval_dashscope.py <audio_dir> <sentences.tsv> <out.json> [hotword_vocabulary_id]
"""
import sys, os, glob, json, time, wave, threading
import dashscope
from dashscope.audio.asr import Recognition, RecognitionCallback, RecognitionResult
from cer import cer, contains_all

KEYS = ["小七", "李四", "Alice", "王明", "张伟", "陈晨", "流式", "熔断", "受限级"]

class CB(RecognitionCallback):
    def __init__(self):
        self.first_partial = None; self.final_at = None; self.sentences = []; self.t_start = None; self.done = threading.Event()
    def on_open(self): pass
    def on_event(self, result: RecognitionResult):
        now = time.perf_counter()
        s = result.get_sentence()
        if s and s.get("text"):
            if self.first_partial is None: self.first_partial = now
            if RecognitionResult.is_sentence_end(s):
                self.sentences.append(s["text"]); self.final_at = now
    def on_complete(self): self.done.set()
    def on_error(self, result):
        # SDK 的 RecognitionResult.__str__ 自己会崩(找 headers 属性),只取字段
        info = {k: getattr(result, k, None) for k in ("status_code", "code", "message", "request_id")}
        print("error:", info, file=sys.stderr); self.error = info; self.done.set()
    def on_close(self): self.done.set()

def main():
    audio_dir, tsv, out = sys.argv[1:4]
    vocab = sys.argv[4] if len(sys.argv) > 4 else None
    refs = dict(line.rstrip("\n").split("\t", 1) for line in open(tsv, encoding="utf-8") if line.strip())
    rows = []
    for path in sorted(glob.glob(os.path.join(audio_dir, "*.wav"))):
        sid = os.path.basename(path).split("_")[0]
        with wave.open(path, "rb") as w:
            sr = w.getframerate(); pcm = w.readframes(w.getnframes())
        dur = len(pcm) / 2 / sr
        cb = CB()
        kw = dict(model="paraformer-realtime-v2", format="pcm", sample_rate=sr, callback=cb, language_hints=["zh", "en"])
        if vocab: kw["vocabulary_id"] = vocab
        rec = Recognition(**kw)
        t0 = time.perf_counter(); cb.t_start = t0
        rec.start()
        step = int(sr * 0.1) * 2
        cb.error = None
        for i in range(0, len(pcm), step):
            if cb.error:
                break
            try:
                rec.send_audio_frame(pcm[i:i + step])
            except Exception as e:  # 识别已经停了(通常是 on_error 之后)
                print("send failed:", e, file=sys.stderr); break
            time.sleep(0.1)
        t_end_audio = time.perf_counter()
        try:
            rec.stop()
        except Exception as e:
            print("stop failed:", e, file=sys.stderr)
        cb.done.wait(timeout=20)
        if cb.error:
            raise SystemExit(f"DashScope 识别失败:{cb.error}")
        hyp = "".join(cb.sentences)
        ref = refs.get(sid, "")
        rows.append({"file": os.path.basename(path), "dur_s": round(dur, 2),
                     "first_partial_s": round(cb.first_partial - t0, 3) if cb.first_partial else None,
                     "final_after_audio_end_s": round((cb.final_at or time.perf_counter()) - t_end_audio, 3),
                     "cer": round(cer(ref, hyp), 3) if ref else None,
                     "keys": contains_all(hyp, [k for k in KEYS if k in ref]) if ref else {}, "hyp": hyp, "ref": ref})
        print(f"{os.path.basename(path):28s} first={rows[-1]['first_partial_s']} final+{rows[-1]['final_after_audio_end_s']}s cer={rows[-1]['cer']}  {hyp[:60]}", flush=True)
    summary = {"model": "dashscope/paraformer-realtime-v2", "vocab": bool(vocab), "n": len(rows),
               "cer_mean": round(sum(r["cer"] for r in rows if r["cer"] is not None) / max(1, len(rows)), 3),
               "final_latency_mean_s": round(sum(r["final_after_audio_end_s"] for r in rows) / max(1, len(rows)), 3),
               "key_hit_rate": round(sum(sum(r["keys"].values()) for r in rows) / max(1, sum(len(r["keys"]) for r in rows)), 3)}
    json.dump({"summary": summary, "rows": rows}, open(out, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    print(json.dumps(summary, ensure_ascii=False))

if __name__ == "__main__":
    main()
