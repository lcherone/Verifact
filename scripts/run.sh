#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
exec ./target/bundle/Verifact.app/Contents/MacOS/verifact
