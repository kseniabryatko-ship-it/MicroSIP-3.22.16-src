#!/usr/bin/env bash
# Runs inside the container: checks, tests and a Windows build, as
# build-release.ps1 does on Windows. The package lands in /out.
set -euo pipefail
cd /src

step() { printf '\n== %s\n' "$*"; }

step "format"
cargo fmt --all -- --check

step "lint (Linux)"
cargo clippy --locked -p microsip-core --features eframe/x11 --all-targets -- -D warnings

step "lint (Windows)"
cargo clippy --locked --target x86_64-pc-windows-gnu -p microsip-core --all-targets -- -D warnings

step "tests"
cargo test --locked -p microsip-core --features eframe/x11

step "Windows build"
CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_STRIP=symbols cargo build --locked --release \
    --target x86_64-pc-windows-gnu -p microsip-core --bins

step "package"
stamp=$(date -u +%Y%m%d-%H%M%S)
name="microsip-rs-windows-docker-$stamp"
package="/out/$name"
mkdir -p "$package"
release="$CARGO_TARGET_DIR/x86_64-pc-windows-gnu/release"
cp "$release/microsip-gui.exe" "$release/phone-engine.exe" "$package/"
cat > "$package/README.txt" <<TEXT
MicroSIP-Rust for Windows, built in Docker ($stamp UTC).

microsip-gui.exe   the phone with its window
phone-engine.exe   the same phone in a console window, for tests

Built with the GNU toolchain (MinGW) instead of Visual Studio; it works the
same. The programs are not signed: Windows SmartScreen or Smart App Control
may ask before the first start.
TEXT
(cd /out && zip -qr "$name.zip" "$name")
sha=$(sha256sum "/out/$name.zip" | cut -d' ' -f1)
printf '\nPASS: checks, tests and the Windows build.\n'
printf 'Folder: %s\nZIP: %s.zip\nSHA256: %s\n' "$name" "$name" "$sha"
