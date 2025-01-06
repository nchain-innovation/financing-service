#!/bin/bash

# This build file creates the rust financing service for use with the nchain rnd prototyping projects 
# For a faster build, you can use the cloud builder (cloud-nchain-rndprototyping).  
# Please check the allowed build minutes, as exceeding them may affect ability to build.
# Uncomment the --builder flag to enable the cloud builder, and comment out the --platform flag.

# Project Id:  (financing-service)

# Tags
BASE_TAG=financing-service
VERSION=v2.1
PUBLISH_TAG=nchain/innovation-$BASE_TAG:$VERSION

# multi build, tag and push base images
# docker buildx build --builder cloud-nchain-rndprototyping --platform linux/amd64,linux/arm64 --push -t $PUBLISH_TAG .
docker buildx build  --platform linux/amd64,linux/arm64 --push -t $PUBLISH_TAG .