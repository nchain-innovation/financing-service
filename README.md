# Financing Service - Rust

This is a version of the Financing Service written in Rust.

Many of the elements of this service are common with the Financing Service written in Python which can be found under

https://bitbucket.stressedsharks.com/projects/SDL/repos/financing-service

## To Build the Service
The service is developed in Rust.
The best way to install Rust is to use `rustup`, see https://www.rust-lang.org/tools/install

To build:
```bash
cargo build
```

## To Run the Service
To run:
```bash
cargo run
```
## Supported endpoints
The service provides the following endpoints:
### Service status
/status

Returns a JSON service status report
```JSON
curl http://127.0.0.1:8080/status
{
    "blockchain_status": "Connected",
    "blockchain_update_time": "2022-10-12 10:20:41",
    "clients":[
        {"client_id": "id1", "confirmed":9565721, "unconfirmed": 0},
        {"client_id": "id2", "confirmed":4713, "unconfirmed": 0}
    ]
}
```

### Client Balance
/balance/{client_id}
```JSON
curl http://127.0.0.1:8080/balance/id1
{
    "status": "Success",
    "Balance": {"client_id": "id1", "confirmed":9565721, "unconfirmed": 0}
}
```

### Fund Transactions
/fund/{client_id}/{satoshi}/{no_of_outpoints}/{multiple_tx}/{locking_script}
```JSON
curl -X POST http://127.0.0.1:8080/fund/id1/123/1/false/0000
{
    "status": "Success",
    "outpoints": [
        {
            "hash": "e6d71bb86e514c75921a032a0c7783bc1fab4b1b19fd675cfb3f0b918a3460a8",
            "index": 1
        }
    ]
}
```



## Source code documentation



To generate source code documentation
```bash
cargo doc
```
This will output documentation to `./target/doc/financing_service_rust/index.html`
[Here ](./target/doc/financing_service_rust/index.html)
