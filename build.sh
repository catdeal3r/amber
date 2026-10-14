#!/usr/bin/env bash
#
# Build amber shell binaries in release mode and collect the results, ready to
# run, in a single output directory:
#
#     ./build.sh                       # whole workspace -> ./dist
#     ./build.sh amber-panel           # just the panel -> ./dist
#     ./build.sh amber-panel amber-locker
#     ./build.sh -o /tmp/out amber-locker
#
# With no binary names the whole workspace is built and the output directory is
# refreshed from scratch. When one or more binaries are named, only those
# packages are built and only those binaries are (re)patched, so iterating on a
# single component leaves the rest of the output directory untouched.
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

# Binary name -> cargo package.
all_bins=(amber-panel amber-wallpaper amber-launcher amber-notifd amber-locker amber-media)
declare -A package_of=(
    ["amber-panel"]=panel
    ["amber-wallpaper"]=wallpaper
    ["amber-launcher"]=launcher
    ["amber-notifd"]=notifd
    ["amber-locker"]=locker
    ["amber-media"]=media
)

usage() {
    cat <<EOF
Usage: ./build.sh [options] [binary ...]

Build amber binaries and patch them for the Nix devshell's libraries.

With no binary names the whole workspace is built into the output directory.
Otherwise only the named binaries are rebuilt and patched, leaving every other
binary in the output directory as-is.

Options:
  -o, --out DIR   output directory (default: dist)
  -h, --help      show this help

Binaries:
  ${all_bins[*]}
EOF
}

out=dist
bins=()

while (($#)); do
    case "$1" in
        -o | --out)
            out="${2:?missing value for $1}"
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        -*)
            echo "unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
        *)
            bins+=("$1")
            shift
            ;;
    esac
done

if ((${#bins[@]})); then
    for bin in "${bins[@]}"; do
        if [ -z "${package_of[$bin]:-}" ]; then
            echo "unknown binary: $bin" >&2
            echo "known binaries: ${all_bins[*]}" >&2
            exit 2
        fi
    done
fi

if ((${#bins[@]} == 0)); then
    echo "Building release binaries (whole workspace)..."
    cargo build --release --locked --workspace
    bins=("${all_bins[@]}")
    rm -rf -- "$out"
else
    echo "Building release binaries: ${bins[*]}"
    args=()
    for bin in "${bins[@]}"; do
        args+=(-p "${package_of[$bin]}")
    done
    cargo build --release --locked "${args[@]}"
fi

# The devshell exports the library search path that the binaries need at runtime.
libs="${LD_LIBRARY_PATH%:}"

mkdir -p -- "$out"

for bin in "${bins[@]}"; do
    # Unlink first: a running binary can't be overwritten in place, but it can
    # be replaced (the live process keeps the old inode).
    rm -f -- "$out/$bin"
    cp -- "target/release/$bin" "$out/$bin"
    patchelf --set-rpath "$libs" "$out/$bin"
done

echo
echo "Built into ./$out:"
for bin in "${bins[@]}"; do
    printf '  %s\n' "$out/$bin"
done
