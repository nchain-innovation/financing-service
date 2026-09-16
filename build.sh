#!/bin/bash

# The build fetches uls-client from the private mapi-lite repo over SSH, so it
# needs BuildKit and your ssh-agent forwarded into the dependency-fetch step:
#   eval "$(ssh-agent)" && ssh-add     # once per shell, if not already running
# The key itself never enters the image. See docs/Dependencies.md.
DOCKER_BUILDKIT=1 docker build --ssh default --tag "financing-service-rust" --file Dockerfile .
