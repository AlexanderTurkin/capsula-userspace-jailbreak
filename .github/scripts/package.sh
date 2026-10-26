#!/usr/bin/env bash
# Package the release archive for the current matrix job.
# Env: TARGET, ARCHIVE (tar.gz|zip), TAG; DRIVER_URL/DRIVER_SHA256 on Windows.
set -euo pipefail

tag="${TAG#v}"          # 0.1.0
name="capsula-jb-${TAG}-${TARGET}"
stage="staging/${name}"

rm -rf staging dist
mkdir -p "${stage}" dist

bin="target/${TARGET}/release/capsula-jb"
if [[ "$TARGET" == *-windows-* ]]; then
    bin="${bin}.exe"
fi
cp "$bin" "${stage}/"
cp README.md LICENSE "${stage}/"
cp resources/packaging/QUICKSTART.txt "${stage}/"

case "$TARGET" in
    *-windows-*)
        # Rockchip USB drivers (WinUSB for VID 2207): downloaded and
        # SHA256-verified at build time, shipped inside the archive.
        curl -fsSL "$DRIVER_URL" -o "${stage}/RockchipUSBDriver.zip"
        echo "${DRIVER_SHA256}  ${stage}/RockchipUSBDriver.zip" | sha256sum -c
        cp resources/packaging/windows-DRIVER-SETUP.txt "${stage}/DRIVER-SETUP.txt"
        ;;
    *-linux-*)
        cp resources/udev/99-capsula-jailbreak.rules "${stage}/"
        ;;
esac

case "$ARCHIVE" in
    tar.gz)
        tar -C staging -czf "dist/${name}.tar.gz" "${name}"
        ;;
    zip)
        # bsdtar (macOS/Windows runners) picks the format by extension
        tar -C staging -a -cf "dist/${name}.zip" "${name}"
        ;;
esac

sum_tool="sha256sum"
command -v sha256sum >/dev/null 2>&1 || sum_tool="shasum -a 256"
$sum_tool dist/* | tee "dist/${name}-SHA256SUMS.txt"

ls -la dist/
