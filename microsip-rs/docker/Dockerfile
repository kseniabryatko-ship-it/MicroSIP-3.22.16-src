# Builds MicroSIP-Rust for Windows inside a Linux container, without Rust,
# Visual Studio or anything else installed on the computer. The source is
# not copied into the image: it is mounted when the container runs (see
# docker-build.cmd and docker/README.md).

FROM rust:1.99-bookworm

# MinGW links the Windows programs; ALSA is needed by the Linux test build;
# zip packs the result.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        gcc-mingw-w64-x86-64 libasound2-dev pkg-config zip \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add x86_64-pc-windows-gnu \
    && rustup component add clippy rustfmt

# Build output stays in a volume, away from the Windows `target` folder.
ENV CARGO_TARGET_DIR=/target

WORKDIR /src
CMD ["bash", "/src/docker/build.sh"]
