# Build stage
FROM rust:1.77 as builder
WORKDIR /app

# Cache dependencies
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main(){}" > src/main.rs && cargo build --release && rm -rf src

# Build actual source
COPY src ./src
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim
WORKDIR /app
ENV PORT=5000
ENV STORE_DIR=/tmp/chip8_store

# Install minimal runtime deps
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/chipi-chipi-chapa-chapa /usr/local/bin/chip8-service

EXPOSE 5000

CMD ["/usr/local/bin/chip8-service"]
