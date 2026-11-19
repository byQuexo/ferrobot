#!/usr/bin/env bash
# Builds the Firecracker guest root filesystem (ext4) from the runner image.
# Linux only; run as root so file owners survive into the image.
#
#   sudo scripts/firecracker/build-rootfs.sh [OUT.ext4] [SIZE]   # default rootfs.ext4 2G
#
# Uses FERROBOT_RUNNER_IMAGE (default ferrobot-runner:dev), built from
# runner/Dockerfile if missing.
# TODO(kvm): the resulting image has never booted.
set -euo pipefail

if [ "$(uname -s)" != Linux ]; then
  echo "build-rootfs.sh runs on Linux only (mkfs.ext4 -d)" >&2
  exit 1
fi
if [ "$(id -u)" != 0 ]; then
  echo "run as root: the image must keep root and node file owners" >&2
  exit 1
fi

cd "$(dirname "$0")/../.."
out=${1:-rootfs.ext4}
size=${2:-2G}
image=${FERROBOT_RUNNER_IMAGE:-ferrobot-runner:dev}

if ! docker image inspect "$image" >/dev/null 2>&1; then
  docker build -f runner/Dockerfile -t "$image" .
fi
# The guest init needs `ip` for the MMDS route; the runner image has none.
docker build -q -t "$image-firecracker" - >/dev/null <<EOF
FROM $image
USER root
RUN apt-get update \\
    && apt-get install -y --no-install-recommends iproute2 \\
    && rm -rf /var/lib/apt/lists/*
EOF

work=$(mktemp -d)
cid=$(docker create "$image-firecracker")
trap 'docker rm -f "$cid" >/dev/null; rm -rf "$work"' EXIT

docker export "$cid" | tar -x -C "$work"
# Resolve /sbin (usr/sbin in Debian) inside $work, never on the host.
install -m 0755 scripts/firecracker/ferrobot-init \
  "$work/$(readlink "$work/sbin" || echo sbin)/ferrobot-init"
# Docker fills these at run time; the export leaves them empty.
rm -f "$work/.dockerenv"
printf '127.0.0.1 localhost\n::1 localhost\n' >"$work/etc/hosts"

rm -f "$out"
truncate -s "$size" "$out"
mkfs.ext4 -q -F -d "$work" "$out"
echo "rootfs: $out"
