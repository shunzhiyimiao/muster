#!/usr/bin/env bash
set -euo pipefail
cd ~/stt-eval
python3 -m venv venv
./venv/bin/pip install -q --upgrade pip
./venv/bin/pip install -q --only-binary=:all: sherpa-onnx numpy soundfile dashscope opencc-python-reimplemented || ./venv/bin/pip install -q --only-binary=:all: sherpa-onnx numpy soundfile dashscope
./venv/bin/python -c "import sherpa_onnx; print('sherpa-onnx', sherpa_onnx.__version__)"
cd models
base=https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models
for m in sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20 sherpa-onnx-streaming-zipformer-zh-14M-2023-02-23 sherpa-onnx-streaming-paraformer-bilingual-zh-en sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17; do
  if [ -d "$m" ]; then echo "have $m"; continue; fi
  echo "downloading $m"; curl -sSL --retry 3 -o "$m.tar.bz2" "$base/$m.tar.bz2" && tar xjf "$m.tar.bz2" && rm -f "$m.tar.bz2" && du -sh "$m"
done
echo SETUP_DONE
