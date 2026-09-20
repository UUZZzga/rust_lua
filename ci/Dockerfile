# Drone CI 构建镜像
# 在 Drone 宿主机上预构建一次 (drone-runner-docker 直接使用本地镜像，无需镜像仓库):
#   docker build -t lua-ci:latest -f ci/Dockerfile .
FROM rust:1.98.1-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential cmake curl unzip perl ca-certificates \
      git iproute2 procps \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /drone/src