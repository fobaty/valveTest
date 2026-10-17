#!/usr/bin/env bash
#
# Build, convert to UF2 (with the RP2350 family id), and copy to a Pico 2 W
# that is in BOOTSEL mode (mounted as /Volumes/RP2350).
#
# Usage:
#   1. Hold BOOTSEL, plug in USB (or press RESET while holding BOOTSEL).
#   2. ./flash.sh
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
ELF="$ROOT/target/thumbv8m.main-none-eabihf/release/throttle-valve-controller"
UF2_RAW="/tmp/valve.uf2"
UF2_OUT="/tmp/valve2350.uf2"

cargo build --release --manifest-path "$ROOT/Cargo.toml"

# elf2uf2-rs hardcodes the RP2040 family id (0xe48bff56); the RP2350 bootloader
# requires 0xe48bff57, so patch the family id in every UF2 block.
elf2uf2-rs "$ELF" "$UF2_RAW"
python3 - "$UF2_RAW" "$UF2_OUT" <<'PY'
import struct, sys
src, dst = sys.argv[1], sys.argv[2]
d = bytearray(open(src, "rb").read())
n = len(d) // 512
patched = 0
for i in range(n):
    off = i * 512
    m0, m1 = struct.unpack_from("<II", d, off)
    if m0 == 0x0A324655 and m1 == 0x9E5D5157:
        struct.pack_into("<I", d, off + 28, 0xE48BFF57)
        patched += 1
open(dst, "wb").write(d)
print(f"patched family id in {patched}/{n} UF2 blocks")
PY

DEST="$(ls -d /Volumes/RP2350* /Volumes/RPI-RP2* 2>/dev/null | head -1 || true)"
if [ -z "$DEST" ]; then
    echo "error: BOOTSEL volume not found. Hold BOOTSEL and reconnect the Pico 2 W." >&2
    exit 1
fi

# Clean any previous files so the bootloader is not confused by stale ones.
rm -f "$DEST"/valve*.uf2 "$DEST"/._valve*.uf2 2>/dev/null || true
cp "$UF2_OUT" "$DEST/"
sync
echo "Flashed to $DEST — the board will reboot and run the firmware."
