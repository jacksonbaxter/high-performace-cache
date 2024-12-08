# Build stage
FROM rust:latest as builder

WORKDIR /usr/src/app

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Create src layout
RUN mkdir -p src/bin

# Copy source code
COPY src/main.rs src/main.rs
COPY src/bin/load_test.rs src/bin/load_test.rs

# Build application
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    openssl \
    libssl3 \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Copy binaries from builder
COPY --from=builder /usr/src/app/target/release/server /usr/local/bin/
COPY --from=builder /usr/src/app/target/release/load_test /usr/local/bin/

# Create directory for the application
WORKDIR /app

# Copy .env file (will be overridden by environment variables in docker-compose)
COPY .env ./

EXPOSE 8080

# Default to running the server, can be overridden in docker-compose
CMD ["server"]