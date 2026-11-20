#!/usr/bin/env bash
# Builds the Leptos frontend (web/dist) on Vercel's Linux build image, so a Git push deploys the
# full app without uploading a locally built web/dist. Referenced by vercel.json (buildCommand).
# The Rust functions in api/ are built separately by Vercel's own Rust runtime.
#
# Design notes (all checked 2026-11-08):
#  - Trunk's default `x86_64-unknown-linux-gnu` release needs GLIBC 2.39, newer than Vercel's build
#    image, so it cannot start there. The `musl` release is static and has no glibc requirement.
#    The two tools Trunk downloads itself (wasm-bindgen musl, binaryen wasm-opt) are static too.
#  - Vercel's image already has rustup and Rust 1.92.0, so only the wasm32 target is missing.
set -euo pipefail

TOOLCHAIN="1.92.0"      # same compiler Vercel's Rust runtime uses for api/*.rs
TRUNK_VERSION="0.22.1"  # same Trunk as local development

# If anything fails, say exactly which command and where (the Vercel log otherwise shows only "exited with 1").
trap 'code=$?; echo "!! vercel-build.sh FAILED at line $LINENO (exit $code): $BASH_COMMAND" >&2' ERR

echo "== environment"
uname -srm || true
(ldd --version 2>&1 | head -1) || true
echo "HOME=$HOME  PWD=$PWD"
export PATH="$HOME/.cargo/bin:$PATH"
command -v rustup || echo "(rustup not on PATH yet)"

if ! command -v rustup >/dev/null 2>&1; then
  echo "== installing rustup"
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
fi
rustup --version || true

echo "== installing Rust $TOOLCHAIN with the wasm32 target"
# rustup can return non-zero for harmless post-install reasons: verify the result instead of trusting the code.
rustup toolchain install "$TOOLCHAIN" --profile minimal --target wasm32-unknown-unknown \
  || echo "(rustup returned non-zero; verifying the installation instead)"
export RUSTUP_TOOLCHAIN="$TOOLCHAIN"   # scoped to this script only
rustc --version
cargo --version
rustup target list --toolchain "$TOOLCHAIN" --installed | grep -q '^wasm32-unknown-unknown$' \
  || { echo "!! wasm32-unknown-unknown target is NOT installed for $TOOLCHAIN" >&2; exit 1; }

if ! command -v trunk >/dev/null 2>&1; then
  echo "== installing Trunk $TRUNK_VERSION (static musl build)"
  mkdir -p "$HOME/.cargo/bin"
  curl -sSfL "https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-x86_64-unknown-linux-musl.tar.gz" \
    | tar -xz -C "$HOME/.cargo/bin" trunk
  chmod +x "$HOME/.cargo/bin/trunk"
fi
trunk --version

echo "== building the frontend"
cd web
trunk build --release

test -f dist/index.html || { echo "!! build produced no web/dist/index.html" >&2; exit 1; }
ls -la dist
echo "== frontend build finished"
