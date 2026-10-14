# A Linux bench machine with every linker the harness compares: the default
# (cc with GNU ld), rust-lld, mold and wild, plus clippy, cargo-llvm-cov and
# sccache. Built and run by `python3 bench/bench.py docker -- <benchmark ...>`.
#
# Versions are pinned and every download is checked against the SHA-256
# recorded here, for both architectures Docker runs on.
FROM rust:1.99-bookworm

ARG TARGETARCH
ARG MOLD=3.0.0
ARG WILD=0.10.0
ARG LLVM_COV=0.9.1
ARG SCCACHE=0.18.0

RUN apt-get update \
 && apt-get install -y --no-install-recommends clang python3 perl git ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && rustup component add clippy llvm-tools-preview \
 && git config --global safe.directory '*'

RUN set -eu; \
    case "$TARGETARCH" in \
      arm64) arch=aarch64; \
             mold_sum=52c759d3689babaea4c42af2f31062a74ab83e17ffb5a8090232534c67b70577; \
             wild_sum=e9d670e41f76481a68984f816e25bd2f124664db3ac935053e1a6fc41d2894c2; \
             cov_sum=abf5f13c1520f8756d2192bfaaeadb0208f5b7eaa8cadc15bf847befa42b7360; \
             sccache_sum=2b3284d5da3b46a47dc4229e75bb7b88ac4aa99c8d754fb7d2f84997e5a4354a ;; \
      amd64) arch=x86_64; \
             mold_sum=6c90d4a474c7c0409dfb575be03a5345878ac14fdba18de8b40fa58c60121189; \
             wild_sum=641265506a7c06cfb03181b8916ab663ec8407855db6d4db7f8450667d105283; \
             cov_sum=b3f68e625481fed9b16444174f3fa5ebcdbde4a1878803a35eabe2dcefcdc41a; \
             sccache_sum=45f1447fbe231e3037bde351ef70677dd212216c8d62ae7ca409fecc4d6acc89 ;; \
      *) echo "unsupported architecture $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    fetch() { curl -sSfL -o /tmp/t.tar.gz "$1"; echo "$2  /tmp/t.tar.gz" | sha256sum -c -; }; \
    fetch "https://github.com/rui314/mold/releases/download/v${MOLD}/mold-${MOLD}-${arch}-linux.tar.gz" "$mold_sum"; \
    tar xzf /tmp/t.tar.gz -C /usr/local --strip-components 1; \
    fetch "https://github.com/wild-linker/wild/releases/download/${WILD}/wild-linker-${WILD}-${arch}-unknown-linux-gnu.tar.gz" "$wild_sum"; \
    tar xzf /tmp/t.tar.gz -C /usr/local/bin --strip-components 1 "wild-linker-${WILD}-${arch}-unknown-linux-gnu/wild"; \
    fetch "https://github.com/taiki-e/cargo-llvm-cov/releases/download/v${LLVM_COV}/cargo-llvm-cov-${arch}-unknown-linux-gnu.tar.gz" "$cov_sum"; \
    tar xzf /tmp/t.tar.gz -C /usr/local/cargo/bin; \
    fetch "https://github.com/mozilla/sccache/releases/download/v${SCCACHE}/sccache-v${SCCACHE}-${arch}-unknown-linux-musl.tar.gz" "$sccache_sum"; \
    tar xzf /tmp/t.tar.gz -C /usr/local/bin --strip-components 1 "sccache-v${SCCACHE}-${arch}-unknown-linux-musl/sccache"; \
    rm /tmp/t.tar.gz; \
    mold --version; wild --version; cargo llvm-cov --version; sccache --version
