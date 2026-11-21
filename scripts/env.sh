#!/bin/sh
# Source from the repository root; workspace-local Rust takes precedence when present.
if [ -d "$PWD/.tools/cargo/bin" ]; then
    export CARGO_HOME="$PWD/.tools/cargo"
    export RUSTUP_HOME="$PWD/.tools/rustup"
    export PATH="$CARGO_HOME/bin:$PATH"
fi
export CEF_PATH="$PWD/.tools/cef"
if [ -d "$PWD/.tools/build-tools/bin" ]; then
    export PATH="$PWD/.tools/build-tools/bin:$PATH"
fi
# This CLT installation keeps libc++ headers in the SDK but clang does not
# discover them automatically. Scope the header path to this project's builds.
if [ "$(uname -s)" = Darwin ]; then
    VERIFACT_SDK_PATH="$(xcrun --show-sdk-path)"
    case ":${CPLUS_INCLUDE_PATH:-}:" in
        *":$VERIFACT_SDK_PATH/usr/include/c++/v1:"*) ;;
        *) export CPLUS_INCLUDE_PATH="$VERIFACT_SDK_PATH/usr/include/c++/v1${CPLUS_INCLUDE_PATH:+:$CPLUS_INCLUDE_PATH}" ;;
    esac
fi
