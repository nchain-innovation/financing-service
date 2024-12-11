FROM rust:bookworm AS builder

COPY ./data /app/data
COPY . /app/rust

WORKDIR /app/rust
RUN cargo install --path . --root /app

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y \
    libssl3 \
    ca-certificates \
    curl \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/bin/financing-service /app/bin/financing-service
COPY --from=builder /app/data /app/bin/data
WORKDIR /app/bin

# env var to detect we are in a docker instance
ENV APP_ENV=docker
CMD [ "/app/bin/financing-service"]