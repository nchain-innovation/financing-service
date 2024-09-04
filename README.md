# Financing Service - Rust

This is a version of the Financing Service written in Rust.

The Financing Service (FS) creates Bitcoin SV transaction outpoint(s) of the correct satoshi value to fund particular transactions, on request.

The FS is a component that can be used in different applications. Initially these will be R&D applications. The component should be flexible, robust, clearly documented and maintainable, so that it is capable of supporting nChain release products.


![Diagram 1](docs/diagrams/overview.png)

Diagram 1 - Financing Service Overview

As shown in diagram 1 the FS provides an interface that the other application components interface with and uses the blockchain to create the funding transaction outpoints.

The service reads its configuration on startup.

The service uses the `chain-gang` library to interface with the BSV blockchain.


## Use cases

![Diagram 2](docs/diagrams/use-case.png)

Diagram 2 - Financing Service Use Cases

The Financing Service use cases are:
* `Request Transaction Fund` - the FS receives a request for a satoshi value, it creates a funding transaction and provides the outpoint to the requestor so that they can fund their transaction.
* `Request Transaction Funds` - the FS receives a request for multiple outpoints for  a satoshi value, it creates a funding transaction and returns the outpoints.
* `Get Balance` - the FS returns the current level of funding associated with a particular client.
`Top-up Balance` - The Admin will provide a funding transaction to increase the satoshi that the FS can use for funding.
* `Get Status` - the FS will return the current status of the component.



## Geting Started

The project can either be run as an executable or in a docker container.


## Docker
Encapsulating the service in Docker removes the need to install the project dependencies on the host machine.
Only Docker is required to build and run the service.
### 1) Build The Docker Image
To build the docker image associated with the service run the following comand in the project directory.
```bash
./build.sh
```
This builds the Docker image `financing-service-rust`.
### 2) To Run the Image
The to start the Docker container:
```bash
./run.sh
```
This will provide a REST API at http://localhost:8080


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

### Locking scripts
This section contains notes on generating locking scripts for the `fund` call.

A pay to public key hash (P2PKH) unlocking script has the following form
```
OP_DUP OP_HASH160 b467faf0ef536db106d67f872c448bcaccb878c9 OP_EQUALVERIFY OP_CHECKSIG
```
This when encoded into hex has the form
```
76 A9 xxx 88 AC -> 76a9b467faf0ef536db106d67f872c448bcaccb878c988ac
```
So the fund call is expecting the script as a hex string, in this case `76a9b467faf0ef536db106d67f872c448bcaccb878c988ac`. 

### Locking scripts - Python 
In the MOPEngine code we have:
*  `engine/standard_scripts.py` is `p2pkh_script()` - this creates a P2PKH unlocking script for the given public key
* `script.raw_serialize()` method returns the script as bytes, without the prepended length.
* There is a `bytes.hex()` method which converts bytes to hex string.


So the call to setup the unlocking script is `p2pkh_script(public_key_cert).raw_serialize().hex()`


### Locking scripts - Rust
In Rust use the `chain-gang` library and the following code: 
``` rust
    // create the locking script for this wallet keypair
    pub fn locking_script(&self) -> Script {
        let address = &hash160(&self.public_key_as_bytes()).0;

        let mut script = Script::new();
        script.append(OP_DUP);
        script.append(OP_HASH160);
        script.append_data(address);
        script.append(OP_EQUALVERIFY);
        script.append(OP_CHECKSIG);
        script
    }

    pub fn locking_script_as_bytes(&self) -> Vec<u8> {
        self.locking_script().0
    }

    pub fn locking_script_as_hexstr(&self) -> String {
        let bytes = self.locking_script_as_bytes();
        hex::encode(bytes)
    }
```





