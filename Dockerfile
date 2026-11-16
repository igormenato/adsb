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
RUN cargo install cargo-zigbuild --version "${CARGO_ZIGBUILD_VERSION}" --locked \
 && rm -rf /usr/local/cargo/registry /usr/local/cargo/git
RUN groupadd --gid 1000 trck \
 && useradd --uid 1000 --gid 1000 --create-home --shell /bin/bash trck \
 && mkdir -p /src/target /home/trck/.platformio /usr/local/cargo/registry /usr/local/cargo/git \
 && chown -R trck:trck /src /home/trck/.platformio /usr/local/cargo/registry /usr/local/cargo/git
USER trck

FROM base AS chef
ARG CARGO_CHEF_VERSION=0.1.78
USER root
RUN cargo install cargo-chef --version "${CARGO_CHEF_VERSION}" --locked \
 && rm -rf /usr/local/cargo/registry /usr/local/cargo/git \
 && mkdir -p /usr/local/cargo/registry /usr/local/cargo/git \
 && chown -R trck:trck /usr/local/cargo/registry /usr/local/cargo/git
USER trck

FROM chef AS planner
COPY --chown=trck:trck . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS deps
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --recipe-path recipe.json --locked --all-targets \
 && cargo chef cook --recipe-path recipe.json --locked --release --zigbuild --target aarch64-unknown-linux-musl

FROM deps AS build
RUN rm -rf src
COPY --chown=trck:trck rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY --chown=trck:trck src ./src
RUN mkdir out
RUN cargo zigbuild --release --locked --target aarch64-unknown-linux-musl \
 && cp target/aarch64-unknown-linux-musl/release/trck /src/out/trck

FROM deps AS test
RUN rm -rf src
COPY --chown=trck:trck . .
RUN cargo fmt --check \
 && cargo clippy --locked --all-targets -- -D warnings \
 && cargo test --locked \
 && make -C esp32/host test

FROM base AS pio-tools
ARG PIO_VERSION=6.2.0
USER root
RUN pip3 install --break-system-packages --no-cache-dir "platformio==${PIO_VERSION}"
USER trck

FROM pio-tools AS pio
COPY --chown=trck:trck esp32/pio/platformio.ini ./esp32/pio/platformio.ini
RUN pio pkg install -d esp32/pio
COPY --chown=trck:trck esp32/pio ./esp32/pio
RUN pio run -d esp32/pio

FROM pio-tools AS dev
CMD ["bash"]

FROM scratch AS artifact
COPY --from=build /src/out/trck /trck
