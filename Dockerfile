# syntax=docker/dockerfile:1
FROM rust:bookworm AS builder

# uls-client and uls-core come from the private mapi-lite repo over SSH, so the
# build needs git and an SSH client, github.com in known_hosts, and the host's
# ssh-agent forwarded with `docker build --ssh default` (see build.sh). The
# key never enters the image: BuildKit only lends the agent socket to the one
# RUN step that fetches dependencies.
RUN apt-get update && apt-get install -y --no-install-recommends \
    git \
    openssh-client \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p -m 0700 /root/.ssh \
    && ssh-keyscan github.com >> /root/.ssh/known_hosts

COPY ./data /app/data
COPY . /app/rust

WORKDIR /app/rust
RUN --mount=type=ssh cargo install --path . --root /app

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y \
    libssl3 \
    ca-certificates \
    curl \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/bin/financing-service /app/bin/financing-service
# COPY --from=builder /app/data /app/bin/data
RUN mkdir /app/bin/data
WORKDIR /app/bin

# env var to detect we are in a docker instance
ENV APP_ENV=docker
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD curl -f http://127.0.0.1:8080/health || exit 1
CMD [ "/app/bin/financing-service"]
