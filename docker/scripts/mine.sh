#!/bin/bash
#
# Mine blocks on the regtest chain immediately.
#
#   ./mine.sh [count]
#
# Regtest produces no blocks on its own, so anything waiting on a confirmation
# waits forever until a block is mined. Defaults to a single block.
#
# Requires a wild-bit-lab node to be running -- see docker/README.md.
#
# Configuration (all optional, shown with their defaults):
#
#   NODE_CONTAINER=node1             node container to mine on
#   NODE_RPC_PORT=18332
#   NODE_RPC_USER=bitcoin
#   NODE_RPC_PASSWORD=bitcoin
#   MINE_ADDRESS=                    coinbase recipient; a new wallet address if unset
#   WOC_URL=http://localhost:5010    local WoC, reported for indexing lag

set -euo pipefail

NODE_CONTAINER=${NODE_CONTAINER:-node1}
NODE_RPC_PORT=${NODE_RPC_PORT:-18332}
NODE_RPC_USER=${NODE_RPC_USER:-bitcoin}
NODE_RPC_PASSWORD=${NODE_RPC_PASSWORD:-bitcoin}
MINE_ADDRESS=${MINE_ADDRESS:-}
WOC_URL=${WOC_URL:-http://localhost:5010}

usage() {
    cat >&2 <<USAGE
Usage: $(basename "$0") [count]

Mines [count] blocks (default 1) on the regtest chain, right away.

Examples:
  $(basename "$0")                      # mine 1 block
  $(basename "$0") 10                   # mine 10 blocks
  $(basename "$0") 101                  # mature a coinbase output
  MINE_ADDRESS=<addr> $(basename "$0")  # pay the coinbase to a specific address
USAGE
    exit 2
}

die() {
    echo "error: $*" >&2
    exit 1
}

case "${1:-}" in
    -h | --help) usage ;;
esac

COUNT=${1:-1}

if ! echo "$COUNT" | grep -Eq '^[1-9][0-9]*$'; then
    die "count must be a positive whole number, got '$COUNT'"
fi

# Run bitcoin-cli inside the node container.
bcli() {
    docker exec "$NODE_CONTAINER" /app/bitcoin-cli \
        -regtest \
        "-rpcuser=$NODE_RPC_USER" \
        "-rpcpassword=$NODE_RPC_PASSWORD" \
        "-rpcport=$NODE_RPC_PORT" \
        "$@"
}

command -v docker >/dev/null || die "docker is required"

bcli getblockcount >/dev/null 2>&1 ||
    die "cannot reach the node in container '$NODE_CONTAINER'
Is wild-bit-lab running? Override with NODE_CONTAINER=<name>."

before=$(bcli getblockcount)

address=$MINE_ADDRESS
if [ -z "$address" ]; then
    address=$(bcli getnewaddress) || die "getnewaddress failed"
fi

# generatetoaddress returns a JSON array of the hashes it produced.
hashes=$(bcli generatetoaddress "$COUNT" "$address") || die "generatetoaddress failed"

after=$(bcli getblockcount)
mined=$((after - before))

if [ "$mined" -eq 1 ]; then
    echo "mined 1 block to $address"
else
    echo "mined $mined blocks to $address"
fi
echo "height: $before -> $after"

# Show the tip, and the first hash too when a range was mined.
first_hash=$(echo "$hashes" | grep -o '"[0-9a-f]\{64\}"' | head -1 | tr -d '"')
tip_hash=$(echo "$hashes" | grep -o '"[0-9a-f]\{64\}"' | tail -1 | tr -d '"')
if [ "$mined" -gt 1 ] && [ -n "$first_hash" ] && [ "$first_hash" != "$tip_hash" ]; then
    echo "first:  $first_hash"
fi
[ -n "$tip_hash" ] && echo "tip:    $tip_hash"

# The WoC stack indexes a few seconds behind the node. Report the gap rather
# than waiting for it -- this script is meant to return immediately.
woc_blocks=$(curl -s --max-time 5 "$WOC_URL/chain/info" 2>/dev/null |
    grep -o '"blocks":[0-9]*' | head -1 | cut -d: -f2 || true)
if [ -n "$woc_blocks" ]; then
    if [ "$woc_blocks" -lt "$after" ]; then
        echo "woc:    $woc_blocks (still indexing, node is at $after)"
    else
        echo "woc:    $woc_blocks (up to date)"
    fi
fi
