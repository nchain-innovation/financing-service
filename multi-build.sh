#!/bin/bash
#
# Build the multi-architecture image and push it to Docker Hub.
#
# The tag is the version in Cargo.toml, not a hard-coded one (CS-527), and an
# existing tag is never overwritten: an image someone has pulled by tag must
# stay the image that tag names. Bump the version to publish again.
#
# For a faster build, you can use the cloud builder (cloud-nchain-rndprototyping).
# Please check the allowed build minutes, as exceeding them may affect ability
# to build. Set BUILDER=cloud-nchain-rndprototyping to use it.
#
# `--ssh default` forwards your ssh-agent into the dependency fetch: uls-client
# comes from the private mapi-lite repo over SSH (see docs/Dependencies.md).
set -euo pipefail

BASE_TAG=financing-service
VERSION="v$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
PUBLISH_TAG="nchain/innovation-$BASE_TAG:$VERSION"

if [ "$VERSION" = "v" ]; then
    echo "Could not read the version from Cargo.toml." >&2
    exit 1
fi

# What is pushed should be exactly a commit someone can find.
if [ -n "$(git status --porcelain)" ]; then
    echo "The working tree has uncommitted changes; commit or stash them before publishing." >&2
    exit 1
fi

if docker buildx imagetools inspect "$PUBLISH_TAG" >/dev/null 2>&1; then
    echo "$PUBLISH_TAG is already published, and is not overwritten." >&2
    echo "Bump the version in Cargo.toml to publish a new image." >&2
    exit 1
fi

BUILDER_ARGS=()
if [ -n "${BUILDER:-}" ]; then
    BUILDER_ARGS=(--builder "$BUILDER")
fi

echo "Publishing $PUBLISH_TAG from $(git rev-parse --short HEAD)"
docker buildx build ${BUILDER_ARGS[@]+"${BUILDER_ARGS[@]}"} \
    --platform linux/amd64,linux/arm64 \
    --ssh default \
    --label "org.opencontainers.image.version=$VERSION" \
    --label "org.opencontainers.image.revision=$(git rev-parse HEAD)" \
    --push -t "$PUBLISH_TAG" .
