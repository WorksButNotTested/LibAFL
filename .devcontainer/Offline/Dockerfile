# syntax=docker/dockerfile:1.2
FROM rust:1.76.0 AS libafl-offline

# Install cargo-binstall to download the sccache build
RUN curl -L --proto '=https' --tlsv1.2 -sSf \
    https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash

# install sccache to cache subsequent builds of dependencies
RUN cargo binstall --no-confirm sccache

ENV HOME=/root
ENV SCCACHE_CACHE_SIZE="1G"
ENV SCCACHE_DIR=$HOME/.cache/sccache
ENV RUSTC_WRAPPER="/usr/local/cargo/bin/sccache"
ENV IS_DOCKER="1"
RUN sh -c 'echo set encoding=utf-8 > /root/.vimrc' \
    echo "export PS1='"'[LibAFL \h] \w$(__git_ps1) \$ '"'" >> ~/.bashrc && \
    mkdir ~/.cargo && \
    echo "[build]\nrustc-wrapper = \"${RUSTC_WRAPPER}\"" >> ~/.cargo/config

RUN rustup default nightly
RUN rustup component add rustfmt clippy

# Install clang 18, common build tools
ENV LLVM_VERSION=18
RUN apt update && apt install -y \
    build-essential \
    gdb \
    git \
    wget \
    python3-venv \
    ninja-build \
    lsb-release \
    software-properties-common \
    gnupg \
    cmake

RUN set -ex &&\
    wget https://apt.llvm.org/llvm.sh &&\
    chmod +x llvm.sh &&\
    ./llvm.sh ${LLVM_VERSION}

RUN rustup component add --toolchain nightly rustfmt clippy llvm-tools-preview
RUN cargo binstall --locked cargo-make
RUN wget -O /opt/zlib-1.2.13.tar.gz \
    https://zlib.net/fossils/zlib-1.2.13.tar.gz
RUN wget -O /opt/libpng-1.6.37.tar.gz \
    https://github.com/glennrp/libpng/archive/refs/tags/v1.6.37.tar.gz
ENV LLVM_CONFIG_PATH=/usr/bin/llvm-config-18

WORKDIR /opt/
RUN mkdir /opt/qemu-libafl-bridge/
WORKDIR /opt/qemu-libafl-bridge/
RUN git init
RUN git remote add origin https://github.com/AFLplusplus/qemu-libafl-bridge
RUN git fetch --depth=1 origin 06bf8facec33548840413fba1b20858f58e38e2d
RUN git checkout FETCH_HEAD
ENV LIBAFL_QEMU_DIR /opt/qemu-libafl-bridge/
RUN ./configure \
    --as-shared-lib \
    --target-list=x86_64-linux-user \
    --disable-bsd-user \
    --disable-docs \
    --disable-tests \
    --disable-tools \
    --disable-fdt \
    --disable-system \
    --disable-docs
RUN make -j 16
ENTRYPOINT [ "/bin/bash", "-c" ]
CMD ["/bin/bash"]
