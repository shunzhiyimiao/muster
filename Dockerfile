# muster-server 的生产镜像。
#
# ## 为什么要三段
#
# 服务端把网页参会端**编进了二进制**(`include_str!` 见 routes.rs)——
# 内网/公网都不该依赖 CDN,而多一个静态文件目录就多一处"部署时忘了拷"的
# 失败方式。代价是构建顺序有依赖:先有 web dist,才编得动 Rust。
#
# 第三段用 slim 而不是继续用构建镜像:后者带着整个 Rust 工具链,
# 几百兆全是攻击面。运行时只需要一个二进制和一份根证书。
FROM node:20-slim AS web
WORKDIR /w
COPY apps/web/package.json apps/web/pnpm-lock.yaml* ./
# 不给 `|| pnpm install` 兜底:lockfile 对不上时那会**悄悄装出另一套依赖**,
# 而镜像看起来构建成功了。宁可在这里红,也别让线上跑着一份没人验过的依赖。
RUN corepack enable && pnpm install --frozen-lockfile
COPY apps/web/ ./
RUN pnpm build && cp index.html dist/index.html

FROM rust:1-slim AS build
WORKDIR /s
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY muster-provider muster-provider
COPY muster-route muster-route
COPY muster-audit muster-audit
COPY muster-eval muster-eval
COPY muster-runner muster-runner
COPY muster-gateway muster-gateway
COPY muster-prompt muster-prompt
COPY muster-identity muster-identity
COPY muster-server muster-server
COPY muster-meeting-agent muster-meeting-agent
# include_str! 要的产物。**必须在 cargo build 之前**放进来
COPY --from=web /w/dist apps/web/dist
# 连 bootstrap / admin 一起编:**运维工具必须在镜像里**。
# 不带的话,建第一个账号就得在服务器上装一整套 Rust 工具链再编一遍——
# 而那台机器上本来一个编译器都不需要。
RUN cargo build --release -p muster-server --bins --examples \
 && strip target/release/muster-server target/release/examples/bootstrap target/release/examples/admin

FROM debian:bookworm-slim
# sqlx 走 rustls,不需要 libpq;根证书仍要,否则出站 TLS 一律失败
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# 不用 root 跑。唯一要写的地方是自己的审计链——
# **它必须在卷上**:写进容器层的话,每次重启都从空文件开一条新链,
# 而 append-only 哈希链的全部意义就是连续。断了的链证明不了任何事。
RUN useradd -r -u 10001 muster && mkdir -p /data && chown muster:muster /data
VOLUME ["/data"]
ENV MUSTER_SERVER_AUDIT_DB=/data/muster-server-audit.db
COPY --from=build /s/target/release/muster-server /usr/local/bin/muster-server
COPY --from=build /s/target/release/examples/bootstrap /usr/local/bin/muster-bootstrap
COPY --from=build /s/target/release/examples/admin /usr/local/bin/muster-admin
USER muster
EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/muster-server"]
