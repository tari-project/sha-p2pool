FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

LABEL org.opencontainers.image.source=https://github.com/tari-project/sha-p2pool
LABEL org.opencontainers.image.licenses="BSD"

# Install system dependencies
RUN apt-get update && apt-get -y upgrade \
        && apt-get install -y libclang-dev pkg-config cmake protobuf-compiler \
        libudev-dev

# Builds a cargo-chef plan
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json

# Build profile, release by default
ARG BUILD_PROFILE=release
ENV BUILD_PROFILE=$BUILD_PROFILE

# Extra Cargo flags
ARG RUSTFLAGS=""
ENV RUSTFLAGS="$RUSTFLAGS"

# Extra Cargo features
ARG FEATURES=""
ENV FEATURES=$FEATURES

# Builds dependencies
RUN cargo chef cook --profile $BUILD_PROFILE --features "$FEATURES" --recipe-path recipe.json

# Build application
COPY . .
RUN cargo build --profile $BUILD_PROFILE --features "$FEATURES" --locked --bin sha_p2pool

# ARG is not resolved in COPY so we have to hack around it by copying the
# binary to a temporary location
RUN cp /app/target/$BUILD_PROFILE/sha_p2pool /app/

# Use Ubuntu as the release image
FROM ubuntu:noble AS runtime

RUN apt-get update && apt-get install -y tini curl && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy tari over from the build stage
COPY --from=builder /app/sha_p2pool /usr/local/bin

# Copy licenses
COPY LICENSE-* ./

ENTRYPOINT ["tini", "--", "/usr/local/bin/sha_p2pool"]
