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
`/fund/{client_id}/{satoshi}/{no_of_outpoints}/{multiple_tx}/{locking_script}`
Returns one or more funding transactions based on the request, where the parameters are 
* `client_id` - the client id from which these funds will come
* `satoshi` - the value in satoshi to be funded
* `no_of_outpoints` - the number of funding outpoints to be provided
* `multiple_tx` - whether if there are more than one outpoint they should be in separate txs (true|false)
* `locking_script` - the locking script to be associated with these outpoints
```JSON
curl -X POST http://127.0.0.1:8080/fund/id1/123/1/false/0000

{
    "status": "Success", 
    "outpoints": [{
        "hash": "24ecfdb46cc82fdbc708db3995572eb9fc920863c80856f39bcbf03ba0257fb6", 
        "index": 1}], 
    "tx": "0100000001a2b8865073022e619b9f3fb647f0f382940b69ebfac4e3b845f98eae2233acec000000006b483045022100f5c7334c33280d9ea2c762d50b10ca977ce32b1d109969ef578ec19baa0e801f02203a387744f766e18dada69b88eca50c8573bb35c8720b7214ccd2ec33a039e914412102b02cc8307d68c174135fc320a7af3cb4748e14b1701b76f9498ccaf3ffac55efffffffff026e7f0100000000001976a91404e044fb084b497e20a635bbad95b18506666cbf88ac7b0000000000000002000000000000"
}

```

## Add Client
`/client/{client_id}/{wif}`
Add a dynamic client.
```JSON
curl -X POST http://127.0.0.1:8080/client/client1/cVLcPuZMfnNNca....PUZ4LtnC3MjoLh3piTnX9WCndRqWh
{"status": "Success"}
```

## Delete Client
`/client/{client_id}`
Delete a dynamic client.

```JSON
///     curl -X DELETE http://127.0.0.1:8080/client/client1
```

## Get Address
`/client/{client_id}/address`
Get Address for a particular client_id.

```JSON
curl http://127.0.0.1:8080/client/client1/address

{
    "status": "Success", 
    "info": {
        "client_id": "client1", 
        "address": "mfxjfLTXLUcCxMDojqRejpfKnF9WhRG5BK"
    } 
}
```


## Client Balance
`/client/{client_id}/balance`

This returns the current satoshi balance associated with this `client_id`.
```JSON
curl http://127.0.0.1:8082/client/client1/balance      
{   
    "status": "Success", 
    "Balance": {
        "client_id": "client1", 
        "confirmed":0, 
        "unconfirmed": 0
    } 
}
```


