# 本地流式转写边车(sherpa-onnx + 我们自己的 WebSocket 协议,见 server.py)。
# 取代 2026-10-29 之前的 faster-whisper 容器:whisper 是批处理,每句一个请求,2 核机器上两个人说话就崩;
# 流式 Paraformer 在同一台机器上快 5–7 倍、字错率更低、尾延迟 0.1 秒(docs/转写方案评估-2026-10-30.md)。
#
# 模型在构建期从 sherpa-onnx 的 GitHub release 下载进镜像(约 200MB),运行时**不出网**。
# 换模型:--build-arg STT_MODEL=<release 里的目录名> 并把 STT_MODEL_KIND 配对(paraformer-online | zipformer)。
FROM python:3.12-slim
ARG STT_MODEL=sherpa-onnx-streaming-paraformer-bilingual-zh-en
RUN pip install --no-cache-dir --only-binary=:all: "sherpa-onnx>=1.10" "websockets>=13,<16" numpy
# slim 镜像没有 bzip2,用 python 的 tarfile 解压(评估时踩过)
RUN mkdir -p /models && cd /models \
 && python -c "import urllib.request,sys; urllib.request.urlretrieve('https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/${STT_MODEL}.tar.bz2', 'm.tar.bz2')" \
 && python -c "import tarfile; tarfile.open('m.tar.bz2','r:bz2').extractall('/models')" \
 && rm -f m.tar.bz2 && ls /models
ENV STT_MODEL_DIR=/models/${STT_MODEL} STT_MODEL_KIND=paraformer-online STT_THREADS=1 STT_PORT=9000
RUN useradd -r -u 10003 stt
USER stt
COPY server.py /app/server.py
EXPOSE 9000
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s \
  CMD python -c "import urllib.request,sys; sys.exit(0 if urllib.request.urlopen('http://127.0.0.1:9000/health', timeout=3).status==200 else 1)"
CMD ["python", "/app/server.py"]
