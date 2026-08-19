FROM rust:1.94.1-bookworm

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_HOME=/bosn/cargo-home \
    CARGO_TARGET_DIR=/bosn/target

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        clang \
        lld \
        python3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /reld
COPY . .
