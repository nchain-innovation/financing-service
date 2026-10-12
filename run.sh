#!/bin/bash
#
# Run the image built by build.sh, with ./data as its data directory.
#
# * The port is published on loopback only. Set FS_PUBLISH_ADDRESS (for
#   example to 0.0.0.0) to expose it further, and see docs/Configuration.md
#   about putting the funding endpoints behind something that authenticates.
# * The container runs as you, so it can write ./data -- the dynamic config
#   file lives there -- without that directory being opened up to everyone.
# * Nothing else from the host is mounted. In particular not the Docker
#   socket: the service never uses it, and it would make code execution in
#   the container root on the host (CS-527).
set -euo pipefail

PUBLISH_ADDRESS="${FS_PUBLISH_ADDRESS:-127.0.0.1}"

docker run -it \
    --user "$(id -u):$(id -g)" \
    -p "${PUBLISH_ADDRESS}:8080:8080" \
    --mount type=bind,source="$(pwd)"/data,target=/app/bin/data \
    --rm financing-service-rust \
    "$@"
