# -*- coding: utf-8 -*-
"""Muster 的本地流式转写边车:sherpa-onnx 流式识别 + WebSocket,一个说话人一条连接,端点落句。

为什么自己写这 200 行而不用 sherpa-onnx 自带的 websocket 服务:
  - 线协议归我们定(见 muster-provider/src/speech_stream.rs 的模块文档),Rust 客户端只认这一份契约,
    不随上游示例的字段漂移;
  - 过载要**降级不崩**:每条连接前面一个有界队列,塞满就丢音频并告诉客户端丢了多少毫秒,
    而不是无限缓冲到内存涨爆或延迟拖到几十秒——2026-10-29 线上就是这么倒的;
  - 每条连接一个解码线程、每线程 1 个 onnxruntime 线程:2 核机器上实测单线程比双线程快。

协议 v1(ws://host:9000/stream):
  客户端 → 第一帧文本 {"v":1,"speaker":"…","lang":"zh","hotwords":["小七",…]}
  客户端 → 二进制帧:16 kHz 单声道 PCM,小端 int16,任意大小(建议 100–320 ms)
  客户端 → 文本 {"type":"end"}:把尾巴吐完后关闭
  服务端 → {"type":"ready","model":…,"hotwords":bool}
  服务端 → {"type":"partial","segment":n,"text":…}            (≤ 每 300 ms 一条,文本变了才发)
  服务端 → {"type":"final","segment":n,"text":…,"start_ms":…,"end_ms":…}   (端点落句;时间是本流的音频时钟)
  服务端 → {"type":"dropped","ms":n}                            (过载丢了这么多毫秒,≤ 每秒一条)
  服务端 → {"type":"error","message":…} 然后关闭
  服务端 → {"type":"closed"}                                    (end 之后的正常收尾)
  GET /health → 200 "ok model=<目录名> streams=<当前连接数>"

环境变量:STT_MODEL_DIR(模型目录)、STT_MODEL_KIND(paraformer-online | zipformer)、STT_THREADS(每路线程,默认 1)、
  STT_MAX_STREAMS(并发连接上限,默认 8)、STT_QUEUE_MS(每路积压上限,默认 20000)、
  STT_RULE1_SILENCE / STT_RULE2_SILENCE / STT_RULE3_MIN_UTT(端点规则,秒 / 秒 / 秒,默认 2.4 / 1.0 / 20)、
  STT_PORT(默认 9000)。
"""
import asyncio
import glob
import http
import json
import logging
import os
import queue
import threading
import time

import numpy as np
import sherpa_onnx
import websockets

log = logging.getLogger("stt")

MODEL_DIR = os.environ.get("STT_MODEL_DIR", "/models/sherpa-onnx-streaming-paraformer-bilingual-zh-en")
MODEL_KIND = os.environ.get("STT_MODEL_KIND", "paraformer-online")
THREADS = int(os.environ.get("STT_THREADS", "1"))
MAX_STREAMS = int(os.environ.get("STT_MAX_STREAMS", "8"))
QUEUE_MS = int(os.environ.get("STT_QUEUE_MS", "20000"))
RULE1 = float(os.environ.get("STT_RULE1_SILENCE", "2.4"))
RULE2 = float(os.environ.get("STT_RULE2_SILENCE", "1.0"))
RULE3 = float(os.environ.get("STT_RULE3_MIN_UTT", "20"))
PORT = int(os.environ.get("STT_PORT", "9000"))
SAMPLE_RATE = 16000
PARTIAL_EVERY_S = 0.3
DROPPED_EVERY_S = 1.0


def pick(d, pattern, int8=True):
    """按模式找模型文件。transducer 的解码器必须 fp32(int8 解码器会出大量重复字,评估里 CER 从 12% 变 41%)。"""
    hits = sorted(glob.glob(os.path.join(d, pattern)))
    if not hits:
        raise FileNotFoundError(f"{pattern} in {d}")
    q = [h for h in hits if ("int8" in os.path.basename(h)) == int8]
    return (q or hits)[0]


def build_recognizer():
    tokens = pick(MODEL_DIR, "tokens.txt")
    common = dict(tokens=tokens, num_threads=THREADS, sample_rate=SAMPLE_RATE, feature_dim=80,
                  enable_endpoint_detection=True, rule1_min_trailing_silence=RULE1,
                  rule2_min_trailing_silence=RULE2, rule3_min_utterance_length=RULE3)
    if MODEL_KIND == "paraformer-online":
        rec = sherpa_onnx.OnlineRecognizer.from_paraformer(
            encoder=pick(MODEL_DIR, "encoder*.onnx"), decoder=pick(MODEL_DIR, "decoder*.onnx", int8=False), **common)
        return rec, False
    if MODEL_KIND == "zipformer":
        kw = dict(encoder=pick(MODEL_DIR, "encoder*.onnx"), decoder=pick(MODEL_DIR, "decoder*.onnx", int8=False),
                  joiner=pick(MODEL_DIR, "joiner*.onnx"), decoding_method="modified_beam_search", **common)
        bv = glob.glob(os.path.join(MODEL_DIR, "bpe.vocab"))
        kw.update(modeling_unit="cjkchar+bpe", bpe_vocab=bv[0]) if bv else kw.update(modeling_unit="cjkchar")
        return sherpa_onnx.OnlineRecognizer.from_transducer(**kw), True
    raise SystemExit(f"STT_MODEL_KIND 不认识:{MODEL_KIND}(paraformer-online | zipformer)")


RECOGNIZER, HOTWORDS_OK = build_recognizer()
STREAMS = 0
STREAMS_LOCK = threading.Lock()


class Lane:
    """一条连接:asyncio 侧收音频进有界队列,解码线程从队列取、识别、把事件丢回 asyncio 侧。"""

    def __init__(self, loop, speaker, hotwords):
        self.loop = loop
        self.speaker = speaker
        self.chunks = queue.Queue()
        self.queued_ms = 0.0            # 队列里的音频毫秒数(有界的是它,不是条数)
        self.dropped_ms = 0.0           # 攒着还没通知的丢弃量
        self.last_drop_notice = 0.0
        self.events = asyncio.Queue()
        self.lock = threading.Lock()
        if hotwords and HOTWORDS_OK:
            self.stream = RECOGNIZER.create_stream("\n".join(hotwords))
        else:
            self.stream = RECOGNIZER.create_stream()
        self.thread = threading.Thread(target=self.decode_loop, name=f"stt-{speaker}", daemon=True)

    # ---- asyncio 侧
    def push(self, pcm_bytes):
        ms = len(pcm_bytes) / 2 / SAMPLE_RATE * 1000
        with self.lock:
            if self.queued_ms + ms > QUEUE_MS:
                self.dropped_ms += ms
                now = time.monotonic()
                if now - self.last_drop_notice >= DROPPED_EVERY_S:
                    self.last_drop_notice = now
                    dropped, self.dropped_ms = self.dropped_ms, 0.0
                    self.emit({"type": "dropped", "ms": int(dropped)})
                return
            self.queued_ms += ms
        self.chunks.put((pcm_bytes, ms))

    def end(self):
        self.chunks.put(None)

    def emit(self, msg):
        self.loop.call_soon_threadsafe(self.events.put_nowait, msg)

    # ---- 解码线程
    def decode_loop(self):
        rec, s = RECOGNIZER, self.stream
        pos_ms = 0.0            # 本流已接收的音频时长 = 音频时钟
        segment = 0
        seg_start = None        # 当前段第一次出现文字时的时钟
        last_partial = ("", 0.0)
        try:
            while True:
                item = self.chunks.get()
                if item is None:
                    tail = np.zeros(int(SAMPLE_RATE * 0.6), dtype=np.float32)   # 补一点静音让端点落下
                    s.accept_waveform(SAMPLE_RATE, tail)
                    s.input_finished()
                    while rec.is_ready(s):
                        rec.decode_stream(s)
                    text = rec.get_result(s).strip()
                    if text:
                        # 收尾时才冒出来的尾巴(通常是最后一个字)没有 seg_start:按"最后半秒"算,别让它 start == end
                        start = seg_start if seg_start is not None else max(0.0, pos_ms - 600)
                        self.emit({"type": "final", "segment": segment, "text": text,
                                   "start_ms": int(start), "end_ms": int(pos_ms)})
                    self.emit({"type": "closed"})
                    return
                pcm_bytes, ms = item
                with self.lock:
                    self.queued_ms -= ms
                samples = np.frombuffer(pcm_bytes, dtype=np.int16).astype(np.float32) / 32768.0
                s.accept_waveform(SAMPLE_RATE, samples)
                pos_ms += ms
                while rec.is_ready(s):
                    rec.decode_stream(s)
                text = rec.get_result(s).strip()
                if text and seg_start is None:
                    seg_start = max(0.0, pos_ms - 600)     # 文字出现前大约半秒就在说了
                if rec.is_endpoint(s):
                    if text:
                        self.emit({"type": "final", "segment": segment, "text": text,
                                   "start_ms": int(seg_start if seg_start is not None else pos_ms), "end_ms": int(pos_ms)})
                        segment += 1
                    rec.reset(s)
                    seg_start = None
                    last_partial = ("", time.monotonic())
                    continue
                now = time.monotonic()
                if text and text != last_partial[0] and now - last_partial[1] >= PARTIAL_EVERY_S:
                    last_partial = (text, now)
                    self.emit({"type": "partial", "segment": segment, "text": text})
        except Exception as e:  # 解码线程不能悄悄死:告诉客户端,它会留痕并重连
            log.exception("decode loop crashed")
            self.emit({"type": "error", "message": f"解码失败:{e}"})


async def handle(ws):
    global STREAMS
    loop = asyncio.get_running_loop()
    with STREAMS_LOCK:
        if STREAMS >= MAX_STREAMS:
            await ws.send(json.dumps({"type": "error", "message": f"并发连接已达上限 {MAX_STREAMS}"}, ensure_ascii=False))
            await ws.close()
            return
        STREAMS += 1
    lane = None
    try:
        first = await asyncio.wait_for(ws.recv(), timeout=10)
        if not isinstance(first, str):
            await ws.send(json.dumps({"type": "error", "message": "第一帧必须是配置 JSON"}, ensure_ascii=False))
            return
        cfg = json.loads(first)
        speaker = str(cfg.get("speaker") or "?")
        hotwords = [str(h) for h in (cfg.get("hotwords") or []) if str(h).strip()]
        lane = Lane(loop, speaker, hotwords)
        lane.thread.start()
        await ws.send(json.dumps({"type": "ready", "model": os.path.basename(MODEL_DIR.rstrip("/")),
                                  "hotwords": bool(hotwords and HOTWORDS_OK)}, ensure_ascii=False))
        log.info("stream open speaker=%s hotwords=%d streams=%d", speaker, len(hotwords), STREAMS)

        async def sender():
            while True:
                ev = await lane.events.get()
                await ws.send(json.dumps(ev, ensure_ascii=False))
                if ev["type"] in ("closed", "error"):
                    return

        send_task = asyncio.create_task(sender())
        async for msg in ws:
            if isinstance(msg, bytes):
                lane.push(msg)
            else:
                try:
                    kind = json.loads(msg).get("type")
                except Exception:
                    kind = None
                if kind == "end":
                    lane.end()
                    break
        else:
            lane.end()   # 客户端没说 end 就断了:把尾巴吐完再收
        try:
            await asyncio.wait_for(send_task, timeout=30)
        except asyncio.TimeoutError:
            log.warning("stream flush timed out speaker=%s", speaker)
    except websockets.exceptions.ConnectionClosed:
        if lane is not None:
            lane.end()
    except Exception as e:
        log.exception("handler failed")
        try:
            await ws.send(json.dumps({"type": "error", "message": str(e)}, ensure_ascii=False))
        except Exception:
            pass
    finally:
        with STREAMS_LOCK:
            STREAMS -= 1
        if lane is not None:
            log.info("stream closed speaker=%s streams=%d", lane.speaker, STREAMS)


async def health(connection, request):
    if request.path == "/health":
        body = f"ok model={os.path.basename(MODEL_DIR.rstrip('/'))} streams={STREAMS}\n"
        return connection.respond(http.HTTPStatus.OK, body)
    return None


async def main():
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    logging.getLogger("websockets").setLevel(logging.WARNING)   # /health 会被它记成 "connection rejected (200 OK)",纯噪音
    log.info("sherpa-onnx streaming server: model=%s kind=%s threads/stream=%d max_streams=%d queue_ms=%d",
             MODEL_DIR, MODEL_KIND, THREADS, MAX_STREAMS, QUEUE_MS)
    async with websockets.serve(handle, "0.0.0.0", PORT, process_request=health, max_size=4 * 1024 * 1024,
                                ping_interval=20, ping_timeout=20):
        await asyncio.Future()


if __name__ == "__main__":
    asyncio.run(main())
