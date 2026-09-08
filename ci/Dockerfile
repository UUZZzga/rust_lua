# Drone CI 构建镜像
# 在 Drone 宿主机上预构建一次 (drone-runner-docker 直接使用本地镜像，无需镜像仓库):
#   docker build -t lua-ci:latest -f ci/Dockerfile .
FROM rust:latest

RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential cmake curl unzip perl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
