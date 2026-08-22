# syntax=docker/dockerfile:1.7

# Both upstream images are pinned to immutable multi-platform manifest digests.
# The resulting RC2 image is pushed only to the local qualification registry
# and referenced by its repository digest in the frozen pilot profiles.
FROM node:24-bookworm-slim@sha256:3638d9a6fe4030bd716be989438248074489337ba3275657f93595428be4fc03 AS node-runtime

FROM rust:1.93-bookworm@sha256:7c4ae649a84014c467d79319bbf17ce2632ae8b8be123ac2fb2ea5be46823f31

COPY --from=node-runtime /usr/local/ /usr/local/

# Package-manager caches stay inside container-private writable tmpfs. The
# image root remains read-only and no host credential directory is mounted.
ENV CARGO_HOME=/tmp/cargo \
    npm_config_store_dir=/tmp/pnpm-store \
    PIP_CACHE_DIR=/tmp/pip-cache

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends \
        build-essential \
        ca-certificates \
        git \
        libssl-dev \
        pkg-config \
        python3 \
        python3-pip \
        python3-venv \
    && rm -rf /var/lib/apt/lists/*

RUN npm install --global --ignore-scripts=false \
        @anthropic-ai/claude-code@2.1.223 \
        @openai/codex@0.147.0 \
        pnpm@10.12.1 \
    && npm cache clean --force

COPY codex-forge /usr/local/bin/codex-forge
RUN chmod 0755 /usr/local/bin/codex-forge

RUN rustc --version | grep -F 'rustc 1.93.' \
    && cargo --version \
    && node --version | grep -E '^v24\.' \
    && pnpm --version | grep -Fx '10.12.1' \
    && claude --version | grep -F '2.1.223' \
    && codex-forge --version | grep -F '0.147.0' \
    && python3 --version \
    && git --version

LABEL org.opencontainers.image.title="Forge supervised-pilot agent runtime" \
      org.opencontainers.image.version="1.1.0-rc2" \
      org.opencontainers.image.revision="be770b858e3aeff90618494b4fc0d133333527ef" \
      org.opencontainers.image.qualification.patch-sha256="0338a65d3489e5d7972cd0ce7abd6b6f9a91f53d9bfd3ce8652e853728f884f4"
