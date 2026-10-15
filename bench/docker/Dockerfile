# The Linux image the Docker benchmarks build corpus projects in.
#
# It carries the toolchain the project pins, the system libraries OpenHuman
# links against (the same list as OpenHuman's own Dockerfile), mold, sccache,
# and a
# tinymold binary built from this checkout. Build it from the repository root:
#
#   docker build -f bench/docker/Dockerfile -t tinymold-bench .
#
# bench/docker_openhuman.py does this for you.

ARG RUST_VERSION=1.96.1
FROM rust:${RUST_VERSION}-bookworm AS tinymold
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p tinymold && cp target/release/tinymold /tinymold

FROM rust:${RUST_VERSION}-bookworm
ARG MOLD_VERSION=3.0.0
ARG SCCACHE_VERSION=0.18.0
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake pkg-config libssl-dev libasound2-dev libxdo-dev \
        libxtst-dev libx11-dev libevdev-dev clang ca-certificates git perl \
    && rm -rf /var/lib/apt/lists/*
RUN arch="$(uname -m)" \
    && curl -fsSL "https://github.com/rui314/mold/releases/download/v${MOLD_VERSION}/mold-${MOLD_VERSION}-${arch}-linux.tar.gz" \
       | tar -xz -C /usr/local --strip-components=1 \
    && mold --version
RUN arch="$(uname -m)" \
    && curl -fsSL "https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/sccache-v${SCCACHE_VERSION}-${arch}-unknown-linux-musl.tar.gz" \
       | tar -xz -C /usr/local/bin --strip-components=1 --wildcards '*/sccache' \
    && sccache --version
COPY --from=tinymold /tinymold /usr/local/bin/tinymold
COPY bench/docker/openhuman.sh /opt/bench/openhuman.sh
