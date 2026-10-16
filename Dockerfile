# syntax=docker/dockerfile:1

# Base images are pinned by digest as well as tag (CS-527), so a rebuild uses
# exactly the image that was reviewed, not whatever the tag points at that day.
# The tag is kept for the reader. To move to a newer one, take the index digest
# from `docker buildx imagetools inspect <image:tag>` and update both.
FROM rust:1.98-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS builder

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

# The build context is trimmed by .dockerignore: no .git, no target/, and no
# local data/ files, which can hold runtime clients' WIFs.
COPY . /app/rust

WORKDIR /app/rust
# --locked: build exactly the dependency versions in the committed, audited
# Cargo.lock, and fail rather than resolve new ones (CS-527).
RUN --mount=type=ssh cargo install --locked --path . --root /app

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587
RUN apt-get update && apt-get install -y --no-install-recommends \
    libssl3 \
    ca-certificates \
    curl \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

# An unprivileged user (CS-527). Code execution in the service is then that
# user, not root. The data directory is the only thing it writes.
RUN useradd --system --no-create-home --uid 10001 --user-group fs \
    && mkdir -p /app/bin/data \
    && chown fs:fs /app/bin/data
COPY --from=builder /app/bin/financing-service /app/bin/financing-service
WORKDIR /app/bin
USER fs

# The configuration and the dynamic config file live here. Mount it; a
# host directory must be writable by the user the container runs as (run.sh
# runs it as the invoking user for exactly that reason).
VOLUME ["/app/bin/data"]

# Listen on every interface: the container's own loopback cannot be reached
# from outside it. Which host interfaces see the port is set where it is
# published (run.sh publishes it on the host's loopback). Override it with
# -e FS__LISTEN_ADDR=... like any other setting (CS-624).
ENV FS__LISTEN_ADDR=0.0.0.0:8080
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD curl -f http://127.0.0.1:8080/health || exit 1
CMD [ "/app/bin/financing-service"]
