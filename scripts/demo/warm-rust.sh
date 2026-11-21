#!/usr/bin/env bash
# Runs inside builder's sandbox (as its user) before the take: a minimal Rust toolchain and a shared
# target dir with tokio and backon compiled, so the recorded `cargo run` only builds the example.
set -euo pipefail
command -v cc >/dev/null || { echo "no C linker: add gcc and libc6-dev under Settings, CLI packages"; exit 1; }
[ -x "$HOME/.cargo/bin/cargo" ] ||
  curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal --default-toolchain stable
mkdir -p "$HOME/.cargo"
printf '[build]\ntarget-dir = "%s/.cache/cargo-target"\n' "$HOME" >"$HOME/.cargo/config.toml"
rm -rf /tmp/warm && mkdir -p /tmp/warm/src && cd /tmp/warm
cat >Cargo.toml <<'EOF'
[package]
name = "warm"
version = "0.1.0"
edition = "2021"

[dependencies]
backon = "1"
tokio = { version = "1", features = ["full"] }
EOF
echo 'fn main() {}' >src/main.rs
"$HOME/.cargo/bin/cargo" build -q
rm -rf /tmp/warm
