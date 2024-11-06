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
    "outpoints": [{"hash": ecac3322ae8ef945b8e3c4faeb690b9482f3f047b63f9f9b612e02735086b8a2, "index": 1}]
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
    "Balance": {"client_id": "client1", "address":mfxjfLTXLUcCxMDojqRejpfKnF9WhRG5BK} 
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


