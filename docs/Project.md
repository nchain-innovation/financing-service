
# Project Status
This section contains project status related notes.



## Done
Required to write in Rust
* Need to find WIF library
https://docs.rs/bitcoin/latest/bitcoin/util/key/struct.PrivateKey.html#method.from_wif

* Assume rust-sv does transaction signing - it appears to do so
https://docs.rs/sv/latest/sv/transaction/sighash/fn.sighash.html

* Write Woc interface
https://docs.rs/reqwest/latest/reqwest/


## In Progress
* REST API
* Reorder unspent on insertion

## To Do
* Periodic event
* Manage inserted unspent differently?

* Should use REST error codes for failures
* Should use ? in requests...
* grep for unwraps