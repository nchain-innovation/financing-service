# Financing Service - Configuration

Configuration for this service can be found in the `data\financing-service.toml` toml file.
The toml file is read when the service starts.

The file is composed of the following sections:

## [blockchain_interface]
Configures the blockchain interface to use (in this case WhatsOnChain, connection to testnet).
```TOML
[blockchain_interface]
# For bsv testnet (default)
interface_type = "woc"
network_type = "test"
```
## [web_interface]
Configures the REST API endpoint for the service.
```TOML
[web_interface]
address = '127.0.0.1'
port = 8080
```
## [logging]
Configures the log level for the service.
```TOML
[logging]
log_level = 'info'
```
The logging level can be one of:
* `"error"` - Designates very serious errors.
* `"warn"` - Designates hazardous situations.
* `"info"` - Designates useful information.
* `"debug"` - Designates useful information.
* `"trace"` - Designates very low priority, often extremely verbose, information.


## [service]

This configures the period between requests for the latest UTXO from the blockchain. 

In this case it is set to refresh every 60 seconds.
```TOML
[service]
utxo_refresh_period = 60
```

## [[client]]
Configures each of the clients that the service supports.

Note that these can also be configured using the dynamic config.

```TOML
[[client]]
client_id = "id1"
wif_key = "cW1ciwAgTLs2EGa6cZHpf...kvq72s15rbiUonkrQAhDU4FG"

[[client]]
client_id = "id2"
wif_key = "cRJukFhMkntAdZctwcW6.....GTaBTYwcwStRcwh1rqgJdayZa2"
```
* `client_id` - is how we identify this client
* `wif_key` - is the wallet independent format of the key used to fund this client's transactions.