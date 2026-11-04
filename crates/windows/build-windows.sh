#!/usr/bin/env bash
# Cross-compile the Windows client from Linux with the MinGW toolchain from nixpkgs.
# Output: target/x86_64-pc-windows-gnu/release/screentime-{service,tray}.exe and screentime.exe
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
pthreads="$(nix build --no-link --print-out-paths --inputs-from "$root" nixpkgs#pkgsCross.mingwW64.windows.pthreads)"
nix shell --inputs-from "$root" nixpkgs#pkgsCross.mingwW64.buildPackages.gcc -c env \
  CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
  CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="-L $pthreads/lib" \
  CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc \
  AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar \
  RC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-windres \
  cargo build --manifest-path "$root/Cargo.toml" --release --target x86_64-pc-windows-gnu \
    -p screentime-windows --bin screentime-service --bin screentime-tray \
    -p screentime-client --bin screentime \
    -p screentime-credprov --lib "$@"

# Everything an install needs, in one zip.
dist="$root/target/windows-dist"
rm -rf "$dist" && mkdir -p "$dist/screentime-windows"
cp "$root"/target/x86_64-pc-windows-gnu/release/{screentime,screentime-service,screentime-tray}.exe \
  "$root"/target/x86_64-pc-windows-gnu/release/screentime_credprov.dll \
  "$root"/crates/windows/{install,uninstall}.ps1 "$dist/screentime-windows/"
(cd "$dist" && nix shell --inputs-from "$root" nixpkgs#zip -c zip -qr screentime-windows.zip screentime-windows)
echo "$dist/screentime-windows.zip"
