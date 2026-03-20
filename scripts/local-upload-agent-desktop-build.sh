#!/bin/bash

set -euo pipefail

cd "$(dirname "$0")/.."
source "$HOME/.cargo/env"
pnpm --dir apps/local-upload-agent build
