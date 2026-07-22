#!/bin/bash
# Cloudflare Pages / clean-checkout build. Fail on any error, including inside
# a pipe (a curl failure feeding an installer must not be masked by the
# installer exiting 0).
set -euo pipefail

# Pinned versions/checksums: this script runs unattended on every deploy, so
# fetching "latest" of an installer -- or an unverified game-data blob -- would
# let an upstream change (or compromise) flow straight into the deployed site.
WASM_PACK_VERSION="v0.15.0"
GAME_DATA_URL="https://github.com/valentinps/satisfactorymap/releases/download/game-data-v1/game_data.zip"
GAME_DATA_SHA256="75b2087fae82ff00a1d36fccebd0970550e5ccebfad6d2b159c2838718cd13d6"

echo "Installing Rust..."
# rustup itself is fetched over pinned-TLS; the toolchain is stable (pin it
# with a rust-toolchain.toml if a specific version is ever needed).
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

# Load the Rust environment variables so cargo is on PATH.
source "$HOME/.cargo/env"

echo "Installing wasm-pack ${WASM_PACK_VERSION}..."
# Pinned prebuilt binary (Cloudflare Pages builds on Linux x86_64) rather than
# the "install latest" shell installer -- extract the one binary onto PATH.
WASM_PACK_TARBALL="wasm-pack-${WASM_PACK_VERSION}-x86_64-unknown-linux-musl.tar.gz"
curl --proto '=https' --tlsv1.2 -sSf -L -o "$WASM_PACK_TARBALL" \
  "https://github.com/rustwasm/wasm-pack/releases/download/${WASM_PACK_VERSION}/${WASM_PACK_TARBALL}"
tar -xzf "$WASM_PACK_TARBALL" --strip-components=1 -C "$HOME/.cargo/bin" \
  "wasm-pack-${WASM_PACK_VERSION}-x86_64-unknown-linux-musl/wasm-pack"
rm -f "$WASM_PACK_TARBALL"

echo "Downloading game data from GitHub Releases..."
curl --proto '=https' --tlsv1.2 -sSf -L -o game_data.zip "$GAME_DATA_URL"
echo "${GAME_DATA_SHA256}  game_data.zip" | sha256sum -c -

echo "Unpacking game data..."
python3 game_data/package_game_data.py unpack game_data.zip

echo "Building WASM and static site..."
python3 tools/build_site.py

echo "Build complete! Output is in the dist/ folder."
