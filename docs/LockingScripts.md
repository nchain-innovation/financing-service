# Locking scripts
This document contains information on generating locking scripts for the `fund` call.

A pay to public key hash (P2PKH) unlocking script has the following form
```
OP_DUP OP_HASH160 b467faf0ef536db106d67f872c448bcaccb878c9 OP_EQUALVERIFY OP_CHECKSIG
```
This when encoded into hex has the form
```
76 A9 xxx 88 AC -> 76a9b467faf0ef536db106d67f872c448bcaccb878c988ac
```
So the fund call is expecting the script as a hex string, in this case `76a9b467faf0ef536db106d67f872c448bcaccb878c988ac`. 

## Locking scripts - Python 
In the `tx-engine` code we have:
* `p2pkh_script()` - this creates a P2PKH unlocking script for the given public key hash
* `script.raw_serialize()` method returns the script as bytes, without the prepended length.
* There is a `bytes.hex()` method which converts bytes to hex string.


So the call to setup the unlocking script is 
```Python
from tx_engine import p2pkh_script
unlocking_script = p2pkh_script(public_key_hash).raw_serialize().hex()`
```

## Locking scripts - Rust
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
