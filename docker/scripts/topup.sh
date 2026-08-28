#!/bin/bash
#
# Top up a financing-service client's balance on regtest, creating the client
# first if it does not exist yet.
#
#   ./topup.sh <client_id> [amount_in_bsv]
#
# Requires the `woc` profile and a wild-bit-lab node to be running -- see
# docker/README.md. This only makes sense on regtest: it funds the client by
# spending from the node's own wallet and mining a block to confirm it.
#
# Configuration (all optional, shown with their defaults):
#
#   FS_URL=http://localhost:8081     financing-service REST API
#   NODE_CONTAINER=node1             wild-bit-lab node container to spend from
#   NODE_RPC_PORT=18332
#   NODE_RPC_USER=bitcoin
#   NODE_RPC_PASSWORD=bitcoin
#   FS_ADMIN_API_KEY=                sent as X-API-Key when creating a client
#   FS_CLIENT_API_KEY=               sent as X-API-Key when reading a balance
#   CONFIRM_TIMEOUT=120              seconds to wait for the WoC stack to index

set -euo pipefail

FS_URL=${FS_URL:-http://localhost:8081}
NODE_CONTAINER=${NODE_CONTAINER:-node1}
NODE_RPC_PORT=${NODE_RPC_PORT:-18332}
NODE_RPC_USER=${NODE_RPC_USER:-bitcoin}
NODE_RPC_PASSWORD=${NODE_RPC_PASSWORD:-bitcoin}
FS_ADMIN_API_KEY=${FS_ADMIN_API_KEY:-}
FS_CLIENT_API_KEY=${FS_CLIENT_API_KEY:-}
CONFIRM_TIMEOUT=${CONFIRM_TIMEOUT:-120}

# Coinbase outputs need 100 confirmations before they can be spent, so a wallet
# with nothing mature has to mine this many blocks to free one up.
COINBASE_MATURITY_BLOCKS=101

usage() {
    cat >&2 <<USAGE
Usage: $(basename "$0") <client_id> [amount_in_bsv]

Tops up <client_id> by [amount_in_bsv] (default 1), creating the client with a
freshly generated regtest key if it does not exist.

Examples:
  $(basename "$0") client1              # top up by 1 BSV
  $(basename "$0") client1 25           # top up by 25 BSV
  FS_URL=http://localhost:8080 $(basename "$0") client1
USAGE
    exit 2
}

die() {
    echo "error: $*" >&2
    exit 1
}

[ $# -ge 1 ] || usage
CLIENT_ID=$1
AMOUNT=${2:-1}

case "$CLIENT_ID" in
    -h | --help) usage ;;
esac

# Reject anything that is not a positive decimal, so a typo cannot be passed
# through to sendtoaddress as something surprising.
if ! echo "$AMOUNT" | grep -Eq '^[0-9]+(\.[0-9]{1,8})?$' || [ "$(echo "$AMOUNT" | tr -d '0.')" = "" ]; then
    die "amount must be a positive number with at most 8 decimal places, got '$AMOUNT'"
fi

# --- helpers ---------------------------------------------------------------

# Run bitcoin-cli inside the node container.
bcli() {
    docker exec "$NODE_CONTAINER" /app/bitcoin-cli \
        -regtest \
        "-rpcuser=$NODE_RPC_USER" \
        "-rpcpassword=$NODE_RPC_PASSWORD" \
        "-rpcport=$NODE_RPC_PORT" \
        "$@"
}

# GET $1, setting HTTP_BODY and HTTP_CODE. Never fails the script itself so the
# caller can branch on the status code.
http_get() {
    local path=$1 response
    local -a auth=()
    [ -n "$FS_CLIENT_API_KEY" ] && auth=(-H "X-API-Key: $FS_CLIENT_API_KEY")
    response=$(curl -s -w '\n%{http_code}' --max-time 15 "${auth[@]}" "$FS_URL$path" || true)
    HTTP_CODE=${response##*$'\n'}
    HTTP_BODY=${response%$'\n'*}
    [ -n "$HTTP_CODE" ] || HTTP_CODE=000
}

# POST $1 with JSON body $2, setting HTTP_BODY and HTTP_CODE.
http_post() {
    local path=$1 body=$2 response
    local -a auth=()
    [ -n "$FS_ADMIN_API_KEY" ] && auth=(-H "X-API-Key: $FS_ADMIN_API_KEY")
    response=$(curl -s -w '\n%{http_code}' --max-time 15 -X POST \
        -H 'Content-Type: application/json' "${auth[@]}" \
        -d "$body" "$FS_URL$path" || true)
    HTTP_CODE=${response##*$'\n'}
    HTTP_BODY=${response%$'\n'*}
    [ -n "$HTTP_CODE" ] || HTTP_CODE=000
}

# Pull a string field out of a flat JSON object.
json_str() {
    echo "$1" | grep -o "\"$2\":\"[^\"]*\"" | head -1 | sed "s/.*:\"//; s/\"$//"
}

# Pull a numeric field out of a flat JSON object.
json_num() {
    echo "$1" | grep -o "\"$2\":-\?[0-9]*" | head -1 | cut -d: -f2
}

# Generate a regtest/testnet WIF that the node's wallet does NOT know about.
#
# Deliberately not `getnewaddress` + `dumpprivkey`: that leaves the key in the
# node's wallet, and coin selection will then happily spend this client's coins
# to fund some *other* transaction, silently draining the balance. A key the
# node has never seen cannot be spent by the node.
#
# WIF is base58check over 0xEF || <32-byte key> || 0x01 (compressed pubkey).
generate_wif() {
    python3 -c '
import hashlib, os, sys

ALPHABET = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"

payload = b"\xef" + os.urandom(32) + b"\x01"
raw = payload + hashlib.sha256(hashlib.sha256(payload).digest()).digest()[:4]

n = int.from_bytes(raw, "big")
out = bytearray()
while n:
    n, r = divmod(n, 58)
    out.append(ALPHABET[r])
for byte in raw:
    if byte:
        break
    out.append(ALPHABET[0])
sys.stdout.write(bytes(reversed(out)).decode())
'
}

# Satoshis -> BSV, for display.
sats_to_bsv() {
    awk -v s="$1" 'BEGIN { printf "%.8f", s / 100000000 }'
}

# --- preflight -------------------------------------------------------------

command -v curl >/dev/null || die "curl is required"
command -v docker >/dev/null || die "docker is required"

http_get /status
[ "$HTTP_CODE" = "200" ] || die "financing-service not reachable at $FS_URL (HTTP $HTTP_CODE)
Is it running, and is FS_URL right? The woc profile publishes it on 8081 by default."

chain_status=$(json_str "$HTTP_BODY" blockchain_status)
if [ "$chain_status" != "Connected" ]; then
    echo "warning: blockchain_status is '$chain_status', not 'Connected' -- the top-up may not show up" >&2
fi

bcli getblockcount >/dev/null 2>&1 ||
    die "cannot reach the node in container '$NODE_CONTAINER'
Is wild-bit-lab running? Override with NODE_CONTAINER=<name>."

# --- resolve or create the client ------------------------------------------

http_get "/client/$CLIENT_ID/address"
case "$HTTP_CODE" in
    200)
        address=$(json_str "$HTTP_BODY" address)
        echo "client '$CLIENT_ID' exists at $address"
        ;;
    422)
        echo "client '$CLIENT_ID' not found -- creating it"
        command -v python3 >/dev/null ||
            die "python3 is required to generate a client key"
        wif=$(generate_wif) || die "failed to generate a key"
        new_address=""

        http_post /client "{\"client_id\":\"$CLIENT_ID\",\"wif\":\"$wif\"}"
        if [ "$HTTP_CODE" != "200" ]; then
            [ "$HTTP_CODE" = "401" ] || [ "$HTTP_CODE" = "403" ] &&
                die "not authorised to create a client (HTTP $HTTP_CODE) -- set FS_ADMIN_API_KEY"
            die "failed to create client (HTTP $HTTP_CODE): $HTTP_BODY"
        fi

        http_get "/client/$CLIENT_ID/address"
        [ "$HTTP_CODE" = "200" ] || die "client created but its address is unreadable (HTTP $HTTP_CODE)"
        address=$(json_str "$HTTP_BODY" address)
        echo "created client '$CLIENT_ID' at $address"
        ;;
    *)
        die "unexpected response looking up client (HTTP $HTTP_CODE): $HTTP_BODY"
        ;;
esac

[ -n "$address" ] || die "could not determine the client address"

# --- protect the client's own coins ----------------------------------------

# If the node's wallet also holds the client's key -- which it does whenever the
# key came from `getnewaddress` -- then coin selection is free to fund the
# top-up *from the client's own UTXOs*, and the balance goes down instead of up.
# Lock them for the duration so the wallet has to look elsewhere.
locked_outputs=""
build_locks() {
    # A client with no coins yet produces no matches, and under `set -o pipefail`
    # a grep that matches nothing would abort the script -- hence the `|| true`.
    bcli listunspent 0 9999999 "[\"$address\"]" 2>/dev/null |
        grep -E '"txid"|"vout"' |
        sed 's/.*"txid": "\([^"]*\)".*/\1/; s/.*"vout": \([0-9]*\).*/\1/' |
        paste - - -d, |
        awk -F, '{ printf "%s{\"txid\":\"%s\",\"vout\":%s}", (NR>1 ? "," : ""), $1, $2 }' ||
        true
}

unlock_outputs() {
    if [ -n "$locked_outputs" ]; then
        bcli lockunspent true "[$locked_outputs]" >/dev/null 2>&1 || true
        locked_outputs=""
    fi
}
trap unlock_outputs EXIT

if bcli validateaddress "$address" 2>/dev/null | grep -q '"ismine": true'; then
    echo "warning: the node's wallet holds this client's key, so other wallet activity" >&2
    echo "         can spend its coins. Clients created by this script are safe from that;" >&2
    echo "         ones made with getnewaddress/dumpprivkey are not." >&2
fi

locked_outputs=$(build_locks)
if [ -n "$locked_outputs" ]; then
    if bcli lockunspent false "[$locked_outputs]" >/dev/null 2>&1; then
        echo "locked the client's existing outputs so they are not spent to fund the top-up"
    else
        echo "warning: could not lock the client's outputs; the top-up may spend them" >&2
        locked_outputs=""
    fi
fi

# --- send and confirm ------------------------------------------------------

http_get "/client/$CLIENT_ID/balance"
before=$(json_num "$HTTP_BODY" confirmed)
[ -n "$before" ] || before=0
echo "balance before: $(sats_to_bsv "$before") BSV"

# With the client's coins locked the wallet may have nothing mature left, so a
# failure here is usually "insufficient funds" rather than something fatal.
if ! txid=$(bcli sendtoaddress "$address" "$AMOUNT" 2>&1); then
    echo "node wallet cannot cover $AMOUNT BSV -- mining $COINBASE_MATURITY_BLOCKS blocks"
    bcli generatetoaddress "$COINBASE_MATURITY_BLOCKS" "$(bcli getnewaddress)" >/dev/null ||
        die "failed to mine blocks"
    txid=$(bcli sendtoaddress "$address" "$AMOUNT") ||
        die "sendtoaddress failed even after mining: $txid"
fi
echo "sent $AMOUNT BSV in $txid"

# Regtest mines on demand, so nothing confirms until we say so.
bcli generatetoaddress 1 "$(bcli getnewaddress)" >/dev/null || die "failed to mine the confirming block"
echo "mined 1 block to confirm"

# The balance moves only once chain-listener and utxo-store have indexed the new
# block, which lags the node by a few seconds.
echo -n "waiting for the WoC stack to index"
deadline=$((SECONDS + CONFIRM_TIMEOUT))
after=$before
while [ $SECONDS -lt $deadline ]; do
    http_get "/client/$CLIENT_ID/balance"
    if [ "$HTTP_CODE" = "200" ]; then
        after=$(json_num "$HTTP_BODY" confirmed)
        [ -n "$after" ] || after=$before
        [ "$after" != "$before" ] && break
    fi
    echo -n "."
    sleep 3
done
echo

if [ "$after" = "$before" ]; then
    echo "warning: balance unchanged after ${CONFIRM_TIMEOUT}s -- the transaction is on chain ($txid)" >&2
    echo "         but the WoC stack may still be indexing. Check: docker logs utxo-store" >&2
    exit 1
fi

unlock_outputs

echo "balance after:  $(sats_to_bsv "$after") BSV"
echo "topped up '$CLIENT_ID' by $(sats_to_bsv $((after - before))) BSV"
