#!/usr/bin/env bash
# Build the jailbreak adbd: android-tools 4.2.2 (Debian/Ubuntu packaging)
# + the TCP-alongside-USB patch from patches/. Runs INSIDE the container:
#
#   docker build -t capsula-adbd .
#   docker run --rm -v "$PWD":/work capsula-adbd /work/build.sh
#
# Writes the binary to out/adbd and verifies its ELF requirements.
set -euo pipefail

BASE=https://launchpad.net/ubuntu/+archive/primary/+files
ORIG=android-tools_4.2.2+git20130218.orig.tar.xz
DEB=android-tools_4.2.2+git20130218-3ubuntu41.debian.tar.gz

# arm64 multiarch libs live outside the cross toolchain sysroot
export PKG_CONFIG_LIBDIR=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig

OUT=/work/out
mkdir -p "$OUT"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
cd "$TMP"

curl -fsSL "$BASE/$ORIG" -o orig.tar.xz
curl -fsSL "$BASE/$DEB" -o debian.tar.gz
mkdir src
tar xf orig.tar.xz -C src --strip-components=1
tar xf debian.tar.gz -C src
cd src
# the Debian packaging patches first (quilt series): the buildroot
# patches are written against that state
while read -r q; do
    [ -f "debian/patches/$q" ] || continue
    echo "applying debian/$q"
    patch -p1 --batch --forward < "debian/patches/$q"
done < debian/patches/series
for p in /work/patches/*.patch; do
    echo "applying $(basename "$p")"
    patch -p1 --batch --forward < "$p"
done

CF="$(pkg-config --cflags libcrypto)"
mkdir build-adbd
cd build-adbd
# the full flag set of debian/makefiles/adbd.mk (a command-line CPPFLAGS
# overrides the makefile's own) + the cross lib paths
make -j"$(nproc)" SRCDIR="$PWD/.." -f ../debian/makefiles/adbd.mk \
    CC=aarch64-linux-gnu-gcc \
    CPPFLAGS="-O2 -Wall -Wno-unused-parameter $CF \
              -DADB_HOST=0 -DHAVE_FORKEXEC=1 -D_XOPEN_SOURCE -D_GNU_SOURCE \
              -DALLOW_ADBD_ROOT=1 -DHAVE_SYMLINKS -DBOARD_ALWAYS_INSECURE \
              -DHAVE_TERMIO_H -DADBD_NON_ANDROID \
              -I../core/adbd -I../core/include" \
    LDFLAGS="-L/usr/lib/aarch64-linux-gnu"

aarch64-linux-gnu-strip adbd
cp adbd "$OUT/adbd"

echo "=== verification ==="
aarch64-linux-gnu-objdump -p adbd | grep NEEDED
echo "GLIBC max: $(aarch64-linux-gnu-objdump -T adbd | grep -oE 'GLIBC_2\.[0-9]+' | sort -Vu | tail -1)"
echo "OpenSSL:   $(aarch64-linux-gnu-objdump -T adbd | grep -oE 'OPENSSL_1_1_[0-9]+' | sort -Vu | tr '\n' ' ')"
grep -aq ADBD_TCP_PORT adbd && echo "TCP patch:  present"
ls -la adbd
