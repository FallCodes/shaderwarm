#!/usr/bin/env bash
# Builds upstream Fossilize (MIT) at a pinned commit and installs fossilize_replay plus the
# capture layer into $PREFIX (default: ~/.local/share/shaderwarm/fossilize).
set -euo pipefail

FOSSILIZE_REPO=https://github.com/ValveSoftware/Fossilize.git
FOSSILIZE_COMMIT=556da9ff7d7616704dda515756219ab1dab693ec  # 2026-09-24

ROOT=$(cd "$(dirname "$0")/.." && pwd)
SRC=$ROOT/vendor/fossilize
BUILD=$ROOT/vendor/build/fossilize
PREFIX=${PREFIX:-${XDG_DATA_HOME:-$HOME/.local/share}/shaderwarm/fossilize}

for tool in git cmake ninja c++; do
    command -v "$tool" >/dev/null || { echo "missing build tool: $tool" >&2; exit 1; }
done

if [[ ! -d $SRC/.git ]]; then
    git clone --filter=blob:none "$FOSSILIZE_REPO" "$SRC"
fi
git -C "$SRC" fetch --quiet origin "$FOSSILIZE_COMMIT"
git -C "$SRC" checkout --quiet --detach "$FOSSILIZE_COMMIT"
git -C "$SRC" submodule update --init --recursive --quiet

cmake -S "$SRC" -B "$BUILD" -G Ninja \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$PREFIX" \
    -DCMAKE_INSTALL_BINDIR=bin \
    -DCMAKE_INSTALL_LIBDIR=lib \
    -DFOSSILIZE_VULKAN_LAYER=ON \
    -DFOSSILIZE_CLI=ON \
    -DFOSSILIZE_TESTS=OFF
cmake --build "$BUILD"
cmake --install "$BUILD"

echo "Installed Fossilize to $PREFIX"
