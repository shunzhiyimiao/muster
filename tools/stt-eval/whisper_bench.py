# -*- coding: utf-8 -*-
"""在 whisper 容器里跑:把一组 wav 逐个 POST 到本机 faster-whisper-server,记录耗时与文本(标准库,无依赖)。
用法:python3 whisper_bench.py <audio_dir> <model> <out.json>"""
import sys, os, glob, json, time, uuid, wave, urllib.request
audio_dir, model, out = sys.argv[1:4]
rows = []
for path in sorted(glob.glob(os.path.join(audio_dir, "*.wav"))):
    with wave.open(path, "rb") as w:
        dur = w.getnframes() / w.getframerate()
    data = open(path, "rb").read()
    b = uuid.uuid4().hex.encode()
    body = (b"--" + b + b"\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n" + model.encode() +
            b"\r\n--" + b + b"\r\nContent-Disposition: form-data; name=\"language\"\r\n\r\nzh" +
            b"\r\n--" + b + b"\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.wav\"\r\nContent-Type: audio/wav\r\n\r\n" + data + b"\r\n--" + b + b"--\r\n")
    req = urllib.request.Request("http://127.0.0.1:8000/v1/audio/transcriptions", data=body,
                                 headers={"Content-Type": "multipart/form-data; boundary=" + b.decode()})
    t0 = time.perf_counter()
    try:
        r = urllib.request.urlopen(req, timeout=600); txt = json.loads(r.read().decode())["text"]
    except Exception as e:
        txt = f"<error {e}>"
    dt = time.perf_counter() - t0
    rows.append({"file": os.path.basename(path), "dur_s": round(dur, 2), "proc_s": round(dt, 3), "rtf": round(dt / dur, 3), "hyp": txt.strip()})
    print(f"{os.path.basename(path):24s} rtf={rows[-1]['rtf']:.2f} {txt.strip()[:60]}", flush=True)
json.dump(rows, open(out, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
