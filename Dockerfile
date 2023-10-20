FROM rust:bookworm as builder

COPY ./data /app/data
COPY . /app/rust
COPY ./chain-gang /app/chain-gang

WORKDIR /app/rust
RUN cargo install --path . --root /app

FROM debian:bookworm-slim
RUN apt-get update
RUN apt-get install -y libssl3 ca-certificates
RUN rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/bin/financing-service-rust /app/bin/financing-service-rust
COPY --from=builder /app/data /app/bin/data
WORKDIR /app/bin

# env var to detect we are in a docker instance
ENV APP_ENV=docker
CMD [ "/app/bin/financing-service-rust"]