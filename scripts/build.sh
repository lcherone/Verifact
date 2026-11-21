#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
. ./scripts/env.sh
cargo build --workspace --bins --locked
exec ./target/debug/bundle-verifact
