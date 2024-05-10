#!/bin/bash

# This build file creates the rust financing service for use with the nchain rnd prototyping projects 

# Project Id:  (financing-service)

# Tags
BASE_TAG=financing-service
VERSION=v1.2
PUBLISH_TAG=nchain/rnd-prototyping-$BASE_TAG:$VERSION

# multi build, tag and push base images
# docker buildx build --builder cloud-nchain-rndprototyping --platform linux/amd64,linux/arm64 --push -t $PUBLISH_TAG .
docker buildx build  --platform linux/amd64,linux/arm64 --push -t $PUBLISH_TAG .