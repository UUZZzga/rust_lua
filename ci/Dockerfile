# Drone CI 构建镜像
# 在 Drone 宿主机上预构建一次 (drone-runner-docker 直接使用本地镜像，无需镜像仓库):
#   docker build -t lua-ci:latest -f ci/Dockerfile .
FROM rust:latest

RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential cmake curl unzip perl ca-certificates \
      git iproute2 procps \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install sccache

ENV RUSTC_WRAPPER=/usr/local/cargo/bin/sccache
ENV SCCACHE_DIR=/drone/src/target/sccache
ENV SCCACHE_CACHE_SIZE=2G

WORKDIR /drone/src