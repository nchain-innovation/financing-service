#!/bin/bash

#£docker run -it \
#    -p 8080:8080 \
#    --mount type=bind,source="$(pwd)"/data,target=/app/bin/data \
#    -v /var/run/docker.sock:/var/run/docker.sock \
#    --net=host \
#    --rm financing-service-rust \
#    $1 $2


# Start container
docker run -it \
    -p 8080:8080 \
    --mount type=bind,source="$(pwd)"/data,target=/app/bin/data \
    -v /var/run/docker.sock:/var/run/docker.sock \
    --rm financing-service-rust \
    $1 $2
