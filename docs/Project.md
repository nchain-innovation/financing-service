
# Project Status
This section contains project status related notes.

* Document - September 2022
* Python implementation - October 2022
* Rust implementation - October 2022..October 2023 - Certificate On Chain

## Done
Required to write in Rust
* Need to find WIF library
https://docs.rs/bitcoin/latest/bitcoin/util/key/struct.PrivateKey.html#method.from_wif

* Assume rust-sv does transaction signing - it appears to do so
https://docs.rs/sv/latest/sv/transaction/sighash/fn.sighash.html

* Write Woc interface
https://docs.rs/reqwest/latest/reqwest/


# 12/03/2024 
* version = "0.2.0"
* Added version to status response
* Added tx to the request for a single tx funding


# 02/10/2024
Updates for Overlay Network
* FS - no longer returns the names of clients in status message.
* FS - update version number

## In Progress
* REST API
* Reorder unspent on insertion
* Should use REST error codes for failures

## To Do

* Issue NCH-11485 Unable to build docker images with WARP enabled
https://jira.stressedsharks.com/servicedesk/customer/portal/6/NCH-11485

* Periodic event
* Manage inserted unspent differently?

* Should use ? in requests...
* Grep for unwraps

* Need to think about UTXO management



curl --location --request POST "https://api.whatsonchain.com/v1/bsv/main/tx/raw" \
  --header "Content-Type: application/json" \
  --data "{\"txhex\":\"01000000010bb539b357b85ce468b86a34fa0d6c3587b99a5b68f74159134351dd586ae083000000006a47304402207230d534f77c1fd0953ed20bd5bb6292100e8670860201e245f187cc829cd1da022050e540a76890212a39514f52b0a93ddc60db27640f8b20ea635d938c0d7326954221021abeddfe1373942015c1ef7168dc841d86753431932babdeb2f6e2fccdef882f000000000230f09100000000001976a914b467faf0ef536db106d67f872c448bcaccb878c988ac7b000000000000001876a9b467faf0ef536db106d67f872c448bcaccb878c988ac00000000\" }"

76 a9 b467faf0ef536db106d67f872c448bcaccb878c 988 ac

76 a9 b467faf0ef536db106d67f872c448bcaccb878c 988 ac



01000000010bb539b357b85ce468b86a34fa0d6c3587b99a5b68f74159134351dd586ae083000000006a47304402207230d534f77c1fd0953ed20bd5bb6292100e8670860201e245f187cc829cd1da022050e540a76890212a39514f52b0a93ddc60db27640f8b20ea635d938c0d7326954221021abeddfe1373942015c1ef7168dc841d86753431932babdeb2f6e2fccdef882f000000000230f0910000000000
19 76 a9 14 b4 67 faf0ef536db106d67f872c448bcaccb878c988ac 7b00000000000000
18 76 a9    b4 67 faf0ef536db106d67f872c448bcaccb878c988ac 00000000

a.gordon@8-lm-00250 financing-service-rust % curl -X POST http://127.0.0.1:8080/fund/id1/123/1/false/76a914b467faf0ef536db106d67f872c448bcaccb878c988ac


Python code produces this as tx with input

76a914b467faf0ef536db106d67f872c448bcaccb878c988ac

client_id: id1
satoshi: 123
no_of_outpoints: 1
multiple_tx: false
locking script: 76a914b467faf0ef536db106d67f872c448bcaccb878c988ac

01000000010bb539b357b85ce468b86a34fa0d6c3587b99a5b68f74159134351dd586ae083000000006a47304402205837e2ac79cf5bb2cc9a8aff606de121adc6f12df51c818be3439b643774b1fc02202606a4dadfee1aed6ada6c3bfa1a69e235858a81a6943539b291486736dfe7b94121021abeddfe1373942015c1ef7168dc841d86753431932babdeb2f6e2fccdef882fffffffff0224f09100000000001976a914b467faf0ef536db106d67f872c448bcaccb878c988ac7b000000000000001976a914b467faf0ef536db106d67f872c448bcaccb878c988ac00000000


From rust we get

rust - with unspent satoshi

01000000
01
0bb539b357b85ce468b86a34fa0d6c3587b99a5b68f74159134351dd586ae083
00000000
6a
47
304402206cced183a928447f4f7c71c2e9e4ce5bf269b69d713f23f2f6aa8b6145b5456202204071c4b055739725d6fed5547f972703db8bc3e3a5501d2c21046ddcffa4ee5b41
21021abeddfe1373942015c1ef7168dc841d86753431932babdeb2f6e2fccdef882f
ffff ffff
02
30f0910000000000
1976a914b467faf0ef536db106d67f872c448bcaccb878c988ac
7b00000000000000
1976a914b467faf0ef536db106d67f872c448bcaccb878c988ac
00000000