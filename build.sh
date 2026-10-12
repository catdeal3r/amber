#!/usr/bin/env bash
#
# Build every amber shell binary in release mode and collect the results, ready
# to run, in a single output directory:
#
#     ./build.sh [output-dir]      # defaults to ./dist
#
# The binaries are compiled by cargo, then patched to carry an RPATH pointing at
# the Wayland/GL/fontconfig libraries from the Nix devshell. That lets them be
# executed straight from the output directory (no `cargo run`, no `nix develop`).
set -euo pipefail

# Re-exec inside the flake devshell (which provides cargo, patchelf and the
# runtime libraries) unless we're already in it. The marker guards against an
# infinite loop if the devshell doesn't set anything we could otherwise detect.
if [ -z "${AMBER_IN_DEVSHELL:-}" ]; then
    exec nix develop -c env AMBER_IN_DEVSHELL=1 bash "$0" "$@"
fi

cd "$(dirname -- "$(readlink -f -- "$0")")"

out="${1:-dist}"

bins=(amber-panel amber-wallpaper amber-launcher amber-notifd amber-locker)

echo "Building release binaries..."
cargo build --release --locked --workspace

# The devshell exports the library search path that the binaries need at runtime.
libs="${LD_LIBRARY_PATH%:}"

rm -rf -- "$out"
mkdir -p -- "$out"

for bin in "${bins[@]}"; do
    cp -- "target/release/$bin" "$out/$bin"
    patchelf --set-rpath "$libs" "$out/$bin"
done

echo
echo "Built into ./$out:"
for bin in "${bins[@]}"; do
    printf '  %s\n' "$out/$bin"
done
