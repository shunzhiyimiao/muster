# 转写方案评估工具(2026-10-30)

对比候选转写引擎在**同一组音频**上的实时率(RTF = 处理耗时 / 音频时长)、字错率(CER,繁转简、去标点后算编辑距离)、
关键词命中(人名、别名、领域词)和尾延迟(最后一段音频送完到最终文本)。结论见 `docs/转写方案评估-2026-10-30.md`。

- `sentences.tsv`:参考文本(含「小七,记一下任务,李四负责…」这类会中派活句)。素材用 macOS `say -v Tingting/Meijia` 合成后
  `afconvert -f WAVE -d LEI16@16000 -c 1` 转 16k 单声道;Eddy 读不了中文,别用。合成语音比真人干净,数字只作相对比较。
- `setup_local.sh`:建 venv、装 sherpa-onnx、下载四个模型(两个流式 zipformer、流式 paraformer、SenseVoice)。
- `eval_sherpa.py <model_dir> <kind> <audio_dir> <sentences.tsv> <out.json> [threads] [hotwords.txt]`:按 100ms 分块送入,
  端点检测落句。**transducer 的解码器要用 fp32**(int8 解码器会出大量重复字,CER 从 12% 变 41%)。
- `whisper_bench.py`:在 faster-whisper-server 容器里跑同一组 wav,`score.py` 补 CER。
- `eval_dashscope.py`:DashScope `paraformer-realtime-v2` WebSocket 流式,需 `DASHSCOPE_API_KEY`。
