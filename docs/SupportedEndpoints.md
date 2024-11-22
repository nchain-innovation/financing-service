# Supported endpoints
The service provides the following endpoints:
## Service status
`/status`

This returns the current service status.
```JSON
curl http://127.0.0.1:8080/status
{
    "version": "1.4.0", 
    "blockchain_status": "Connected", 
    "blockchain_update_time": "2024-11-05 14:42:29"
}
```


## Fund Transactions
`/fund`
Returns one or more funding transactions based on the request, where the parameters are 
* `client_id` - the client id from which these funds will come
* `satoshi` - the value in satoshi to be funded
* `no_of_outpoints` - the number of funding outpoints to be provided
* `multiple_tx` - whether if there are more than one outpoint they should be in separate txs (true|false)
* `locking_script` - the locking script to be associated with these outpoints
```JSON

curl -H "Content-Type: application/json" \
     --request POST \
     --data '{"client_id":"client1","satoshi":123,"no_of_outpoints":1,"multiple_tx":false,"locking_script":"000000"}' \
    http://127.0.0.1:8080/fund

{
    "outpoints":  [{"hash": "11e1128551854896dba1af5ebd75f7fb712ae88684cae59e86f89b158de86697", "index": 1}], 
    "txs": [{"tx": "010000000137c36dacc941196a9f773def6e74bc92c8b2952a79178f48b31e4074831c295d000000006a47304402204dde2fda0af07d1c0dc2a22473d1b54065f714405723fdf4a05f27048dc87b770220402ef74c2cc718d83f95a9940f4d489a9931dbbb3eb8d884a38d059e8a63fb2e412102b02cc8307d68c174135fc320a7af3cb4748e14b1701b76f9498ccaf3ffac55efffffffff027f690100000000001976a91404e044fb084b497e20a635bbad95b18506666cbf88ac7b000000000000000300000000000000"}]
    
}   
```

## Add Client
`/client`

Add a dynamic client.

```JSON

curl -H "Content-Type: application/json" \
     --request POST \
     --data '{"client_id":"client15","wif":"cVLcPuZMfnNNcaU...................oLh3piTnX9WCndRqWh"}' \
    http://127.0.0.1:8080/client

{"status": "Success"}
```

## Delete Client
`/client/{client_id}`
Delete a dynamic client.

```JSON
curl -X DELETE http://127.0.0.1:8080/client/client1

{"status": "Success"}
```

## Get Address
`/client/{client_id}/address`
Get Address for a particular client_id.

```JSON
curl http://127.0.0.1:8080/client/client1/address

{
    "address": "mfxjfLTXLUcCxMDojqRejpfKnF9WhRG5BK" 
}   
```


## Client Balance
`/client/{client_id}/balance`

This returns the current satoshi balance associated with this `client_id`.
```JSON
curl http://127.0.0.1:8082/client/client1/balance      
{   
    "confirmed": 99904,
    "unconfirmed": 95162
}
```


