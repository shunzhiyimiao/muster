# -*- coding: utf-8 -*-
"""用 sherpa-onnx 跑一组 wav,模拟实时按 100ms 送入,记录:每句的最终文本、处理耗时/音频时长(RTF)、
端点触发到出结果的延迟、CER、关键词命中。用法:
  python eval_sherpa.py <model_dir> <kind> <audio_dir> <sentences.tsv> <out.json> [threads] [hotwords.txt]
kind: zipformer | paraformer-online | sense-voice | paraformer-offline
"""
import sys, os, glob, json, time, wave, numpy as np
import sherpa_onnx
from cer import cer, contains_all

def read_wav(path):
    with wave.open(path, "rb") as w:
        assert w.getnchannels() == 1 and w.getsampwidth() == 2, path
        sr = w.getframerate()
        data = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(np.float32) / 32768.0
    return sr, data

def pick(d, *pats, int8=True):
    """按模式找模型文件。int8=True 优先量化版(编码器/joiner),int8=False 优先 fp32(解码器:
    sherpa-onnx 文档说明 transducer 的解码器不该用 int8,否则会出重复字)。"""
    for p in pats:
        hits = sorted(glob.glob(os.path.join(d, p)))
        if not hits:
            continue
        q = [h for h in hits if ("int8" in os.path.basename(h)) == int8]
        return (q or hits)[0]
    raise FileNotFoundError(f"{pats} in {d}")

def build(model_dir, kind, threads, hotwords):
    tokens = pick(model_dir, "tokens.txt")
    if kind == "zipformer":
        enc_int8 = os.environ.get("ENC_FP32") is None
        kw = dict(tokens=tokens, encoder=pick(model_dir, "encoder*.onnx", int8=enc_int8),
                  decoder=pick(model_dir, "decoder*.onnx", int8=False), joiner=pick(model_dir, "joiner*.onnx", int8=enc_int8),
                  num_threads=threads, sample_rate=16000, feature_dim=80, enable_endpoint_detection=True,
                  rule1_min_trailing_silence=2.4, rule2_min_trailing_silence=1.2, rule3_min_utterance_length=300)
        if hotwords:
            kw.update(decoding_method="modified_beam_search", hotwords_file=hotwords, hotwords_score=1.5)
            bv = glob.glob(os.path.join(model_dir, "bpe.vocab"))
            kw.update(modeling_unit="cjkchar+bpe", bpe_vocab=bv[0]) if bv else kw.update(modeling_unit="cjkchar")
        return sherpa_onnx.OnlineRecognizer.from_transducer(**kw), True
    if kind == "paraformer-online":
        return sherpa_onnx.OnlineRecognizer.from_paraformer(
            tokens=tokens, encoder=pick(model_dir, "encoder*.onnx"), decoder=pick(model_dir, "decoder*.onnx", int8=False),
            num_threads=threads, sample_rate=16000, feature_dim=80, enable_endpoint_detection=True,
            rule1_min_trailing_silence=2.4, rule2_min_trailing_silence=1.2, rule3_min_utterance_length=300), True
    if kind == "sense-voice":
        return sherpa_onnx.OfflineRecognizer.from_sense_voice(
            model=pick(model_dir, "model*.onnx"), tokens=tokens, num_threads=threads, use_itn=True, language="auto"), False
    if kind == "paraformer-offline":
        return sherpa_onnx.OfflineRecognizer.from_paraformer(
            paraformer=pick(model_dir, "model*.onnx"), tokens=tokens, num_threads=threads), False
    raise SystemExit(f"unknown kind {kind}")

KEYS = ["小七", "李四", "Alice", "王明", "张伟", "陈晨", "流式", "熔断", "受限级"]

def run_online(rec, sr, data, realtime):
    s = rec.create_stream()
    chunk = int(sr * 0.1)
    proc = 0.0
    finals = []
    t_last_audio = None
    for i in range(0, len(data), chunk):
        t0 = time.perf_counter()
        s.accept_waveform(sr, data[i:i + chunk])
        while rec.is_ready(s):
            rec.decode_stream(s)
        if rec.is_endpoint(s):
            txt = rec.get_result(s).strip()
            if txt:
                finals.append(txt)
            rec.reset(s)
        proc += time.perf_counter() - t0
        if realtime:
            time.sleep(max(0.0, 0.1 - (time.perf_counter() - t0)))
    t0 = time.perf_counter()
    tail = np.zeros(int(sr * 0.6), dtype=np.float32)  # 补一点静音让端点落下
    s.accept_waveform(sr, tail)
    s.input_finished()
    while rec.is_ready(s):
        rec.decode_stream(s)
    txt = rec.get_result(s).strip()
    if txt:
        finals.append(txt)
    tail_latency = time.perf_counter() - t0
    proc += tail_latency
    return " ".join(finals), proc, tail_latency

def run_offline(rec, sr, data):
    t0 = time.perf_counter()
    s = rec.create_stream()
    s.accept_waveform(sr, data)
    rec.decode_stream(s)
    dt = time.perf_counter() - t0
    return s.result.text.strip(), dt, dt

def main():
    model_dir, kind, audio_dir, tsv, out = sys.argv[1:6]
    threads = int(sys.argv[6]) if len(sys.argv) > 6 else 2
    hotwords = sys.argv[7] if len(sys.argv) > 7 else None
    refs = dict(line.rstrip("\n").split("\t", 1) for line in open(tsv, encoding="utf-8") if line.strip())
    t0 = time.perf_counter()
    rec, online = build(model_dir, kind, threads, hotwords)
    load_s = time.perf_counter() - t0
    rows = []
    for path in sorted(glob.glob(os.path.join(audio_dir, "*.wav"))):
        sid = os.path.basename(path).split("_")[0]
        sr, data = read_wav(path)
        dur = len(data) / sr
        if online:
            hyp, proc, lat = run_online(rec, sr, data, realtime=False)
        else:
            hyp, proc, lat = run_offline(rec, sr, data)
        ref = refs.get(sid, "")
        rows.append({"file": os.path.basename(path), "dur_s": round(dur, 2), "proc_s": round(proc, 3), "rtf": round(proc / dur, 3),
                     "tail_latency_s": round(lat, 3), "cer": round(cer(ref, hyp), 3) if ref else None,
                     "keys": {k: v for k, v in contains_all(hyp, [k for k in KEYS if k in ref]).items()} if ref else {}, "hyp": hyp, "ref": ref})
        print(f"{os.path.basename(path):28s} rtf={rows[-1]['rtf']:.2f} cer={rows[-1]['cer']}  {hyp[:60]}", flush=True)
    summary = {"model": os.path.basename(model_dir.rstrip('/')), "kind": kind, "threads": threads, "hotwords": bool(hotwords),
               "load_s": round(load_s, 2), "n": len(rows),
               "rtf_mean": round(sum(r["rtf"] for r in rows) / max(1, len(rows)), 3),
               "cer_mean": round(sum(r["cer"] for r in rows if r["cer"] is not None) / max(1, sum(1 for r in rows if r["cer"] is not None)), 3),
               "key_hit_rate": round(sum(sum(r["keys"].values()) for r in rows) / max(1, sum(len(r["keys"]) for r in rows)), 3)}
    json.dump({"summary": summary, "rows": rows}, open(out, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    print(json.dumps(summary, ensure_ascii=False))

if __name__ == "__main__":
    main()
