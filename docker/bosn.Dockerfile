FROM rust:1.95.0-bookworm

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_HOME=/bosn/cargo-home \
    CARGO_TARGET_DIR=/bosn/target \
    PATH=/usr/local/cargo/bin:/bosn/cargo-home/bin:${PATH}

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        clang \
        lld \
        mold \
        python3 \
        time \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /reld
COPY . .
