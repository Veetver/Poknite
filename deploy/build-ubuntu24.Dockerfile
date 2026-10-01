FROM ubuntu:24.04
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends gcc g++ cmake ninja-build pkg-config ca-certificates curl libdbus-1-dev libx11-dev libxext-dev libxft-dev libxrender-dev libxfixes-dev libxcursor-dev libxinerama-dev libxrandr-dev && rm -rf /var/lib/apt/lists/*
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup.sh && sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.99.0 && rm /tmp/rustup.sh
ENV PATH="/root/.cargo/bin:${PATH}"
WORKDIR /work
