# adbd build environment — hermetic, independent of the host.
#
# Debian bullseye on purpose: its cross toolchain targets glibc 2.31,
# and the Capsula firmware ships glibc 2.33 / OpenSSL 1.1 (libcrypto.so.1.1)
# — a binary linked here runs on the stock rootfs as-is. Newer Debian
# bases (2.36+) would produce binaries the stock firmware cannot load.
#
# bullseye has moved to archive.debian.org (LTS ended 2026): the sources
# point there and Release validity checks are off, as usual for the archive.
#
# libssl-dev:arm64 (multiarch) provides the link-time libcrypto.so.1.1;
# the firmware's own copy is used at runtime.
FROM debian:bullseye

RUN printf 'deb http://archive.debian.org/debian bullseye main\n\
deb http://archive.debian.org/debian-security bullseye-security main\n' \
        > /etc/apt/sources.list && \
    dpkg --add-architecture arm64 && \
    apt-get -o Acquire::Check-Valid-Until=false update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        gcc-aarch64-linux-gnu \
        libc6-dev-arm64-cross \
        libssl-dev:arm64 \
        zlib1g-dev:arm64 \
        make patch pkg-config curl ca-certificates xz-utils \
        binutils-aarch64-linux-gnu

WORKDIR /src
