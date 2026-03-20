#!/bin/bash

set -euo pipefail

cd "$(dirname "$0")/.."
source "$HOME/.cargo/env"
pnpm --dir apps/local-upload-agent lint
cargo check --manifest-path apps/local-upload-agent/src-tauri/Cargo.toml
