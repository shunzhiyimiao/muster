# 第三方组件与许可证清单(Third-Party Notices)

本文件为 Muster 依赖的第三方组件清单,满足 Apache-2.0 §4 的归属与声明义务,
并作为隔离网交付的许可证记录(总规划 §8.4 供应链要求 / 任务 P0-02)。

生成方式:`cargo metadata` 全量导出,**非手工维护**——依赖变更后请重新生成。

## 摘要

- Rust 第三方 crate:**192** 个(workspace 内部 crate 不计)

| 许可证 | 数量 |
|---|---|
| MIT OR Apache-2.0 | 119 |
| MIT | 35 |
| Apache-2.0 OR MIT | 8 |
| MIT/Apache-2.0 | 7 |
| Apache-2.0 OR ISC OR MIT | 3 |
| Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | 3 |
| Unlicense OR MIT | 2 |
| ISC | 2 |
| BSD-2-Clause OR Apache-2.0 OR MIT | 2 |
| MIT AND BSD-3-Clause | 1 |
| MIT OR Apache-2.0 OR LGPL-2.1-or-later | 1 |
| (未在 Cargo.toml 声明) | 1 |
| Apache-2.0/MIT | 1 |
| Apache-2.0 OR BSL-1.0 | 1 |
| BSD-3-Clause | 1 |
| Apache-2.0 | 1 |
| Zlib OR Apache-2.0 OR MIT | 1 |
| MIT OR Apache-2.0 OR Zlib | 1 |
| (MIT OR Apache-2.0) AND Unicode-3.0 | 1 |
| MPL-2.0 | 1 |

## 需要人工留意的条目

- **ring**:Cargo.toml 未声明 `license` 字段,其仓库内 LICENSE 为 ISC 风格,
  并含派生自 BoringSSL/OpenSSL 的代码。分发前应随包附带其 LICENSE 原文。
- **r-efi**:三选一授权(MIT / Apache-2.0 / LGPL-2.1-or-later)。
  Muster **选用 MIT 或 Apache-2.0**,不适用 LGPL 条款。该 crate 为 UEFI 目标平台
  的传递依赖,在 macOS/Linux 构建产物中通常不参与链接。
- **webpki-roots(MPL-2.0)**:全清单中唯一的 copyleft 组件。MPL-2.0 是
  **文件级**弱 copyleft——原样引用不影响 Muster 自身的许可选择;但若**修改**
  其源文件,被修改的文件须以 MPL-2.0 开源。结论:**不修改该 crate**,
  需要定制根证书集时改用配置或替换实现,不改其源码。
- **unicode-ident**:含 Unicode-3.0 条款(数据文件),分发时随附其许可证原文。
- **ryu / matchit / subtle**:BSL-1.0 / BSD-3-Clause 系,均为宽松授权,
  义务仅为保留版权与许可声明。

## ⚠️ 待决:Muster 自身的许可证

**Muster 尚未选定自己的许可证**,仓库内没有 `LICENSE` 文件。当前为 private
仓库,不影响使用;但**对外分发、开源或交付客户之前必须先确定**,因为它决定:

- 客户/合作方能否以及如何再分发;
- 是否需要与 Apache-2.0 的 Codex fork 分仓(当前已分仓,兼容性无虞);
- 隔离网交付时安装包的许可声明内容(总规划 P6-05)。

这是**产品与法务决策,不由工程侧代选**。定下来后:补 `LICENSE`、在 README
声明、并把本节替换为正式说明。

## Codex Fork(muster-codex)

`../muster-codex` 是 [openai/codex](https://github.com/openai/codex) 的受控 fork,
遵循 **Apache-2.0**。义务落实:

- 保留上游 `LICENSE` 与 `NOTICE` 原文(未改动);
- fork 的改动范围与同步策略记录在其 `FORK.md`;
- **修改上游文件时必须在文件内加显著修改声明**(Apache-2.0 §4(b));
  当前 fork 仅新增 `FORK.md`,尚未改动任何上游文件。

## 前端依赖

桌面端(`apps/desktop`)的 npm 依赖清单见 `apps/desktop/pnpm-lock.yaml`;
主要为 React(MIT)、Tailwind CSS(MIT)、lucide-react(ISC)、Tauri(MIT OR Apache-2.0)。

## 完整清单

| 组件 | 版本 | 许可证 |
|---|---|---|
| ahash | 0.8.12 | MIT OR Apache-2.0 |
| aho-corasick | 1.1.4 | Unlicense OR MIT |
| android-tzdata | 0.1.1 | MIT OR Apache-2.0 |
| android_system_properties | 0.1.5 | MIT/Apache-2.0 |
| async-trait | 0.1.91 | MIT OR Apache-2.0 |
| autocfg | 1.5.1 | Apache-2.0 OR MIT |
| axum | 0.7.9 | MIT |
| axum-core | 0.4.5 | MIT |
| base64 | 0.22.1 | MIT OR Apache-2.0 |
| bitflags | 2.13.1 | MIT OR Apache-2.0 |
| block-buffer | 0.10.4 | MIT OR Apache-2.0 |
| bumpalo | 3.20.3 | MIT OR Apache-2.0 |
| bytes | 1.12.1 | MIT |
| cc | 1.3.0 | MIT OR Apache-2.0 |
| cfg-if | 1.0.4 | MIT OR Apache-2.0 |
| chrono | 0.4.38 | MIT OR Apache-2.0 |
| core-foundation-sys | 0.8.7 | MIT OR Apache-2.0 |
| cpufeatures | 0.2.17 | MIT OR Apache-2.0 |
| crypto-common | 0.1.7 | MIT OR Apache-2.0 |
| digest | 0.10.7 | MIT OR Apache-2.0 |
| equivalent | 1.0.2 | Apache-2.0 OR MIT |
| errno | 0.3.14 | MIT OR Apache-2.0 |
| fallible-iterator | 0.3.0 | MIT/Apache-2.0 |
| fallible-streaming-iterator | 0.1.9 | MIT/Apache-2.0 |
| fastrand | 2.5.0 | Apache-2.0 OR MIT |
| find-msvc-tools | 0.1.9 | MIT OR Apache-2.0 |
| form_urlencoded | 1.2.2 | MIT OR Apache-2.0 |
| futures | 0.3.33 | MIT OR Apache-2.0 |
| futures-channel | 0.3.33 | MIT OR Apache-2.0 |
| futures-core | 0.3.33 | MIT OR Apache-2.0 |
| futures-executor | 0.3.33 | MIT OR Apache-2.0 |
| futures-io | 0.3.33 | MIT OR Apache-2.0 |
| futures-macro | 0.3.33 | MIT OR Apache-2.0 |
| futures-sink | 0.3.33 | MIT OR Apache-2.0 |
| futures-task | 0.3.33 | MIT OR Apache-2.0 |
| futures-util | 0.3.33 | MIT OR Apache-2.0 |
| generic-array | 0.14.7 | MIT |
| getrandom | 0.2.17 | MIT OR Apache-2.0 |
| getrandom | 0.4.3 | MIT OR Apache-2.0 |
| hashbrown | 0.14.5 | MIT OR Apache-2.0 |
| hashlink | 0.9.1 | MIT OR Apache-2.0 |
| http | 1.4.2 | MIT OR Apache-2.0 |
| http-body | 1.1.0 | MIT |
| http-body-util | 0.1.4 | MIT |
| httparse | 1.10.1 | MIT OR Apache-2.0 |
| httpdate | 1.0.3 | MIT OR Apache-2.0 |
| hyper | 1.3.1 | MIT |
| hyper-rustls | 0.27.2 | Apache-2.0 OR ISC OR MIT |
| hyper-util | 0.1.5 | MIT |
| iana-time-zone | 0.1.65 | MIT OR Apache-2.0 |
| iana-time-zone-haiku | 0.1.2 | MIT OR Apache-2.0 |
| idna | 0.5.0 | MIT OR Apache-2.0 |
| indexmap | 2.2.6 | Apache-2.0 OR MIT |
| ipnet | 2.12.0 | MIT OR Apache-2.0 |
| itoa | 1.0.18 | MIT OR Apache-2.0 |
| js-sys | 0.3.103 | MIT OR Apache-2.0 |
| lazy_static | 1.5.0 | MIT OR Apache-2.0 |
| libc | 0.2.189 | MIT OR Apache-2.0 |
| libsqlite3-sys | 0.28.0 | MIT |
| linux-raw-sys | 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| log | 0.4.33 | MIT OR Apache-2.0 |
| matchers | 0.2.0 | MIT |
| matchit | 0.7.3 | MIT AND BSD-3-Clause |
| memchr | 2.8.3 | Unlicense OR MIT |
| mime | 0.3.17 | MIT OR Apache-2.0 |
| mio | 1.2.2 | MIT |
| nu-ansi-term | 0.50.3 | MIT |
| num-traits | 0.2.19 | MIT OR Apache-2.0 |
| once_cell | 1.21.4 | MIT OR Apache-2.0 |
| percent-encoding | 2.3.2 | MIT OR Apache-2.0 |
| pin-project | 1.1.13 | Apache-2.0 OR MIT |
| pin-project-internal | 1.1.13 | Apache-2.0 OR MIT |
| pin-project-lite | 0.2.17 | Apache-2.0 OR MIT |
| pkg-config | 0.3.33 | MIT OR Apache-2.0 |
| ppv-lite86 | 0.2.21 | MIT OR Apache-2.0 |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 |
| quinn | 0.11.2 | MIT OR Apache-2.0 |
| quinn-proto | 0.11.3 | MIT OR Apache-2.0 |
| quinn-udp | 0.5.2 | MIT OR Apache-2.0 |
| quote | 1.0.47 | MIT OR Apache-2.0 |
| r-efi | 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later |
| rand | 0.8.7 | MIT OR Apache-2.0 |
| rand_chacha | 0.3.1 | MIT OR Apache-2.0 |
| rand_core | 0.6.4 | MIT OR Apache-2.0 |
| regex-automata | 0.4.16 | MIT OR Apache-2.0 |
| regex-syntax | 0.8.11 | MIT OR Apache-2.0 |
| reqwest | 0.12.5 | MIT OR Apache-2.0 |
| ring | 0.17.8 | (未声明,见上) |
| rusqlite | 0.31.0 | MIT |
| rustc-hash | 1.1.0 | Apache-2.0/MIT |
| rustix | 1.1.4 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| rustls | 0.23.10 | Apache-2.0 OR ISC OR MIT |
| rustls-pemfile | 2.1.3 | Apache-2.0 OR ISC OR MIT |
| rustls-pki-types | 1.7.0 | MIT OR Apache-2.0 |
| rustls-webpki | 0.102.4 | ISC |
| rustversion | 1.0.23 | MIT OR Apache-2.0 |
| ryu | 1.0.23 | Apache-2.0 OR BSL-1.0 |
| serde | 1.0.229 | MIT OR Apache-2.0 |
| serde_core | 1.0.229 | MIT OR Apache-2.0 |
| serde_derive | 1.0.229 | MIT OR Apache-2.0 |
| serde_json | 1.0.151 | MIT OR Apache-2.0 |
| serde_path_to_error | 0.1.20 | MIT OR Apache-2.0 |
| serde_spanned | 0.6.9 | MIT OR Apache-2.0 |
| serde_urlencoded | 0.7.1 | MIT/Apache-2.0 |
| sha2 | 0.10.9 | MIT OR Apache-2.0 |
| sharded-slab | 0.1.7 | MIT |
| shlex | 2.0.1 | MIT OR Apache-2.0 |
| signal-hook-registry | 1.4.8 | MIT OR Apache-2.0 |
| slab | 0.4.12 | MIT |
| smallvec | 1.15.2 | MIT OR Apache-2.0 |
| socket2 | 0.5.10 | MIT OR Apache-2.0 |
| socket2 | 0.6.5 | MIT OR Apache-2.0 |
| spin | 0.9.9 | MIT |
| subtle | 2.6.1 | BSD-3-Clause |
| syn | 2.0.119 | MIT OR Apache-2.0 |
| syn | 3.0.3 | MIT OR Apache-2.0 |
| sync_wrapper | 1.0.2 | Apache-2.0 |
| tempfile | 3.27.0 | MIT OR Apache-2.0 |
| thiserror | 1.0.69 | MIT OR Apache-2.0 |
| thiserror-impl | 1.0.69 | MIT OR Apache-2.0 |
| thread_local | 1.1.10 | MIT OR Apache-2.0 |
| tinyvec | 1.12.0 | Zlib OR Apache-2.0 OR MIT |
| tinyvec_macros | 0.1.1 | MIT OR Apache-2.0 OR Zlib |
| tokio | 1.53.1 | MIT |
| tokio-macros | 2.7.1 | MIT |
| tokio-rustls | 0.26.0 | MIT/Apache-2.0 |
| tokio-stream | 0.1.19 | MIT |
| tokio-util | 0.7.19 | MIT |
| toml | 0.8.19 | MIT OR Apache-2.0 |
| toml_datetime | 0.6.11 | MIT OR Apache-2.0 |
| toml_edit | 0.22.20 | MIT OR Apache-2.0 |
| tower | 0.4.13 | MIT |
| tower | 0.5.3 | MIT |
| tower-layer | 0.3.3 | MIT |
| tower-service | 0.3.3 | MIT |
| tracing | 0.1.44 | MIT |
| tracing-attributes | 0.1.31 | MIT |
| tracing-core | 0.1.36 | MIT |
| tracing-log | 0.2.0 | MIT |
| tracing-subscriber | 0.3.23 | MIT |
| try-lock | 0.2.5 | MIT |
| typenum | 1.20.1 | MIT OR Apache-2.0 |
| unicode-bidi | 0.3.18 | MIT OR Apache-2.0 |
| unicode-ident | 1.0.24 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| unicode-normalization | 0.1.25 | MIT OR Apache-2.0 |
| untrusted | 0.9.0 | ISC |
| url | 2.5.0 | MIT OR Apache-2.0 |
| valuable | 0.1.1 | MIT |
| vcpkg | 0.2.15 | MIT/Apache-2.0 |
| version_check | 0.9.5 | MIT/Apache-2.0 |
| want | 0.3.1 | MIT |
| wasi | 0.11.1+wasi-snapshot-preview1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| wasm-bindgen | 0.2.126 | MIT OR Apache-2.0 |
| wasm-bindgen-futures | 0.4.76 | MIT OR Apache-2.0 |
| wasm-bindgen-macro | 0.2.126 | MIT OR Apache-2.0 |
| wasm-bindgen-macro-support | 0.2.126 | MIT OR Apache-2.0 |
| wasm-bindgen-shared | 0.2.126 | MIT OR Apache-2.0 |
| wasm-streams | 0.4.2 | MIT OR Apache-2.0 |
| web-sys | 0.3.103 | MIT OR Apache-2.0 |
| webpki-roots | 0.26.3 | MPL-2.0 |
| windows-core | 0.62.2 | MIT OR Apache-2.0 |
| windows-implement | 0.60.2 | MIT OR Apache-2.0 |
| windows-interface | 0.59.3 | MIT OR Apache-2.0 |
| windows-link | 0.2.1 | MIT OR Apache-2.0 |
| windows-result | 0.4.1 | MIT OR Apache-2.0 |
| windows-strings | 0.5.1 | MIT OR Apache-2.0 |
| windows-sys | 0.48.0 | MIT OR Apache-2.0 |
| windows-sys | 0.52.0 | MIT OR Apache-2.0 |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 |
| windows-targets | 0.48.5 | MIT OR Apache-2.0 |
| windows-targets | 0.52.6 | MIT OR Apache-2.0 |
| windows_aarch64_gnullvm | 0.48.5 | MIT OR Apache-2.0 |
| windows_aarch64_gnullvm | 0.52.6 | MIT OR Apache-2.0 |
| windows_aarch64_msvc | 0.48.5 | MIT OR Apache-2.0 |
| windows_aarch64_msvc | 0.52.6 | MIT OR Apache-2.0 |
| windows_i686_gnu | 0.48.5 | MIT OR Apache-2.0 |
| windows_i686_gnu | 0.52.6 | MIT OR Apache-2.0 |
| windows_i686_gnullvm | 0.52.6 | MIT OR Apache-2.0 |
| windows_i686_msvc | 0.48.5 | MIT OR Apache-2.0 |
| windows_i686_msvc | 0.52.6 | MIT OR Apache-2.0 |
| windows_x86_64_gnu | 0.48.5 | MIT OR Apache-2.0 |
| windows_x86_64_gnu | 0.52.6 | MIT OR Apache-2.0 |
| windows_x86_64_gnullvm | 0.48.5 | MIT OR Apache-2.0 |
| windows_x86_64_gnullvm | 0.52.6 | MIT OR Apache-2.0 |
| windows_x86_64_msvc | 0.48.5 | MIT OR Apache-2.0 |
| windows_x86_64_msvc | 0.52.6 | MIT OR Apache-2.0 |
| winnow | 0.6.26 | MIT |
| winreg | 0.52.0 | MIT |
| zerocopy | 0.8.55 | BSD-2-Clause OR Apache-2.0 OR MIT |
| zerocopy-derive | 0.8.55 | BSD-2-Clause OR Apache-2.0 OR MIT |
| zeroize | 1.7.0 | Apache-2.0 OR MIT |
| zmij | 1.0.23 | MIT |
