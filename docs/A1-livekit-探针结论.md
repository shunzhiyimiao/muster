# A1 探针结论:livekit Rust SDK 音频接口

> 目的:在写 Meeting Agent 之前**摸清 SDK 实际长什么样**,而不是凭印象写。
> 探针代码在 scratchpad(`lk-probe`),**不进产品仓**。

## 一、好消息:重采样不用我们做

```rust
NativeAudioStream::new(track: RtcAudioTrack, sample_rate: i32, num_channels: i32,
                       queue_size_frames: Option<usize>)
```

**调用方直接指定想要的采样率与声道数,SDK 负责转换。** 所以可以直接要
16 kHz 单声道——正是 whisper 想要的格式。原计划里"手写重采样"那一块可以整块划掉。

它实现 `futures::Stream`,产出:

```rust
pub struct AudioFrame<'a> {
    pub data: Cow<'a, [i16]>,   // 交织 PCM
    pub sample_rate: u32,
    pub num_channels: u32,
    pub samples_per_channel: u32,
}
```

i16 PCM,正好是 WAV 的原生格式——**封 WAV 只是加一个 44 字节头**,不需要编解码库。

## 二、说话人归属是白拿的

房间事件:

```rust
RoomEvent::TrackSubscribed { track: RemoteTrack, publication, participant: RemoteParticipant }
```

**每个参会者各一条音轨**,订阅事件里直接带 `participant`。所以"这句话是谁说的"
不需要声纹分离(speaker diarization)——那本来是最难的一块,现在不存在。

而且我们在 `/meetings/:mid/join` 里把 LiveKit 令牌的 `sub` 设成了 **Muster 账号 id**,
所以 `participant.identity()` 直接就是账号 id,转写的说话人能对上人,不用再映射一次。

## 三、代价:构建依赖原生 WebRTC

`livekit` → `libwebrtc` → `webrtc-sys` → `webrtc-sys-build`,而后者在**构建期**
从 GitHub 下载预编译二进制:

```
https://github.com/livekit/rust-sdks/releases/download/{版本}/{目标平台}.zip
```

实测:压缩包 **100 MB+**,本机下载速度约 150 KB/s;`target/` 目录 **1 GB+**。

**这对目标部署形态(企业内网)是要提前解决的**:内网隔离环境构建期访问不了 GitHub。
两条路,都不难,但必须做:

1. 预先下载并缓存到内网制品库,构建时指向它
   (`webrtc-sys-build` 有缓存目录约定,见其 `download_url()` 与锁文件逻辑);
2. 或者在有网机器上构建好 Agent 二进制,内网只部署产物。

对**开发者机器**没有影响——Agent 跑在服务器上,开发机不需要它。

## 四、A3 的风险降了一档

原判断:切片策略是主要技术风险(固定窗口会切断词)。查下来 Rust 侧 VAD 生态可用:

| crate | 说明 |
|---|---|
| `rustvani-vad` | 号称纯 Rust Silero、**无 ONNX 运行时、权重内置** |
| `silero-vad-crs` | Silero C 移植的绑定,零运行时依赖 |
| `webrtc-vad` | 经典轻量方案(注意:我们已经链了 webrtc-sys,需确认不冲突) |

**优先试 `rustvani-vad`**:没有 ONNX 运行时就没有模型下载,内网部署少一个麻烦。
选型要在 A3 实测,不在这里下结论。

## 四点五、**踩到的坑:发布版本自己对不上**

crates.io 上的 livekit 当前**没有任何一组版本能编过**:

| 版本 | 失败原因 |
|---|---|
| `livekit 0.8.1` | `livekit-data-stream 0.1.1` 引用 `livekit-common::{ClientCapability, RemoteParticipantRegistry, CLIENT_PROTOCOL_DATA_STREAM_V2}`,而 `livekit-common` **只发布过 0.1.0,里面没有这三个符号** |
| `livekit 0.7.53` | `livekit-api` 少填 `ConnectWhatsAppCallRequest.wait_until_answered`——protobuf 类型漂移 |

试过钉 `livekit-protocol` 到旧版:不行,它的下限是 `^0.7.10`,而 0.7.10+ 正是引入漂移的那批。

**出路:依赖 git 仓库的固定 commit**(`https://github.com/livekit/rust-sdks`),
那里的 workspace 内部版本是一致的。代价:

- 构建期除了 100MB 的 WebRTC,还要 clone 仓库**及其 submodule**
  (`libyuv` 来自 `chromium.googlesource.com`)——内网隔离环境要多缓存这两样;
- 必须钉 commit,不能跟 `main`:上游随时可能再次失配。

对**内网部署**的结论不变,只是清单更长了:预先缓存 WebRTC 二进制 + livekit 仓库
+ libyuv submodule,或者干脆在有网机器上构建好 Agent 二进制、内网只部署产物。
**后者更省事,而且 Agent 只跑在服务器上,不需要每台开发机都能构建。**

## 五、结论

A1 想验证的四件事,三件已确认、一件待实跑:

| | 结论 |
|---|---|
| 事件形状 | ✅ `TrackSubscribed` 带 participant |
| 帧格式 | ✅ i16 PCM,字段齐全 |
| 能否直接要 16k 单声道 | ✅ 构造参数直接指定,SDK 重采样 |
| 每人一轨 | ✅ 说话人归属白拿,不需要声纹分离 |
| 真机跑通 | ⏳ 待 LiveKit 起来后跑探针确认 |
| 依赖可构建 | ⚠️ crates.io 版本互不兼容,必须走 git + 固定 commit(见第四点五) |

**下一步 A2 可以开工**,不必等真机——接口形状已经确定,写出来的代码不是猜的。
