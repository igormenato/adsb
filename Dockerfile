# syntax=docker/dockerfile:1

ARG RUST_IMAGE=rust:1.99.1-bookworm@sha256:e6f2cff35d0f309acf8433b4f05878ed77092602c5065cc81801177da0bb3a9c

FROM ${RUST_IMAGE} AS base
ARG ZIG_VERSION=0.17.1
ARG CARGO_ZIGBUILD_VERSION=0.24.1
ARG TARGETARCH
WORKDIR /src
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        cmake \
        git \
        pkg-config \
        python3 \
        python3-pip \
        python3-venv \
        xz-utils \
 && rm -rf /var/lib/apt/lists/*
RUN set -eux; \
    case "${TARGETARCH}" in \
      amd64) zig_arch=x86_64; zig_sha=3581863e3cbcfd0fce0efc682ce7e9e93da96668a977791d356a26d2a7e9b17a ;; \
      arm64) zig_arch=aarch64; zig_sha=a1546d5646eb184437918d6635ca6b762a0e889eac09cef720ee7a1ed3fe7795 ;; \
      *) echo "unsupported TARGETARCH=${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    zig_file="zig-${zig_arch}-linux-${ZIG_VERSION}.tar.xz"; \
    curl -fsSL "https://ziglang.org/download/${ZIG_VERSION}/${zig_file}" -o "/tmp/${zig_file}"; \
    echo "${zig_sha}  /tmp/${zig_file}" | sha256sum -c -; \
    mkdir /opt/zig; \
    tar -xJf "/tmp/${zig_file}" --strip-components=1 -C /opt/zig; \
    rm "/tmp/${zig_file}"
ENV PATH="/opt/zig:${PATH}"
COPY rust-toolchain.toml .
RUN rustup component add rustfmt clippy \
 && rustup target add aarch64-unknown-linux-musl
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo install cargo-zigbuild --version "${CARGO_ZIGBUILD_VERSION}" --locked
RUN groupadd --gid 1000 trck \
 && useradd --uid 1000 --gid 1000 --create-home --shell /bin/bash trck \
 && mkdir -p /src/target /home/trck/.platformio /usr/local/cargo/registry /usr/local/cargo/git \
 && chown -R trck:trck /src /home/trck/.platformio /usr/local/cargo/registry /usr/local/cargo/git
USER trck

FROM base AS deps
COPY --chown=trck:trck rust-toolchain.toml Cargo.toml Cargo.lock ./
RUN mkdir src \
 && printf 'fn main() {}\n' > src/main.rs \
 && printf '\n' > src/lib.rs
RUN --mount=type=cache,id=cargo-registry,target=/usr/local/cargo/registry,uid=1000,gid=1000,sharing=locked \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git,uid=1000,gid=1000,sharing=locked \
    cargo fetch --locked

FROM deps AS build
RUN rm -rf src
COPY --chown=trck:trck src ./src
RUN mkdir out
RUN --mount=type=cache,id=cargo-registry,target=/usr/local/cargo/registry,uid=1000,gid=1000,sharing=locked \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git,uid=1000,gid=1000,sharing=locked \
    --mount=type=cache,id=cargo-target,target=/src/target,uid=1000,gid=1000,sharing=locked \
    cargo zigbuild --release --locked --target aarch64-unknown-linux-musl \
 && cp target/aarch64-unknown-linux-musl/release/trck /src/out/trck

FROM deps AS test
RUN rm -rf src
COPY --chown=trck:trck . .
RUN --mount=type=cache,id=cargo-registry,target=/usr/local/cargo/registry,uid=1000,gid=1000,sharing=locked \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git,uid=1000,gid=1000,sharing=locked \
    --mount=type=cache,id=cargo-target,target=/src/target,uid=1000,gid=1000,sharing=locked \
    cargo fmt --check \
 && cargo clippy --locked --all-targets -- -D warnings \
 && cargo test --locked \
 && make -C esp32/host test

FROM base AS pio
ARG PIO_VERSION=6.2.0
USER root
RUN pip3 install --break-system-packages --no-cache-dir "platformio==${PIO_VERSION}"
USER trck
COPY --chown=trck:trck esp32/pio ./esp32/pio
RUN --mount=type=cache,id=pio,target=/home/trck/.platformio,uid=1000,gid=1000,sharing=locked \
    pio run -d esp32/pio

FROM base AS dev
ARG PIO_VERSION=6.2.0
USER root
RUN pip3 install --break-system-packages --no-cache-dir "platformio==${PIO_VERSION}"
USER trck
CMD ["bash"]

FROM scratch AS artifact
COPY --from=build /src/out/trck /trck
