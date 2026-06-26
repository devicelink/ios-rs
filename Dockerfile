FROM --platform=linux/amd64 rust:1-slim-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src src

RUN cargo build --release --bin ios

FROM --platform=linux/amd64 debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends tini \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/ios /usr/local/bin/ios

ENTRYPOINT ["tini", "--", "ios"]
# Listen address set via IOS_TUNNEL_SOCKET_ADDRESS env var in compose
CMD ["tunnel", "daemon"]
