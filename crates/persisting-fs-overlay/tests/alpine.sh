#!/bin/sh

cd "$(dirname "$0")"

set -ex

# Use docker if available, otherwise podman
if command -v docker >/dev/null 2>&1; then
    CONTAINER_RUNTIME=docker
elif command -v podman >/dev/null 2>&1; then
    CONTAINER_RUNTIME=podman
else
    echo "ERROR: neither docker nor podman found" >&2
    exit 1
fi

$CONTAINER_RUNTIME build -t fuse-overlayfs:alpine -f ../Containerfile.alpine ..

$CONTAINER_RUNTIME run --privileged --rm --entrypoint /unlink.sh -w /tmp \
	-e EXPECT_UMOUNT_STATUS=1 \
	-v "$(pwd)/unlink.sh:/unlink.sh" fuse-overlayfs:alpine
