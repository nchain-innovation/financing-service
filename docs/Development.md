# Development
This document contains information useful in installing build tools, building, maintaining and extending this project.

Many of the elements of this service are common with the Financing Service written in Python which can be found under

https://bitbucket.stressedsharks.com/projects/SDL/repos/financing-service

A overview of the project can be found:
https://docs.google.com/document/d/159T_RDgf8CnSq3Kd4PaYgfw9OUrX-kwwdZw4qEe4iP0/edit?usp=sharing


## Rust Installation
This project is build using `Rust`.
The best way to install Rust is to use `rustup`.

To determine the current version of `Rust` run `rustup show`
```bash
$ rustup show
Default host: x86_64-apple-darwin
rustup home:  /Users/a.gordon/.rustup

installed toolchains
--------------------

stable-2022-09-22-x86_64-apple-darwin
stable-x86_64-apple-darwin (default)
nightly-x86_64-apple-darwin

installed targets for active toolchain
--------------------------------------

wasm32-unknown-unknown
x86_64-apple-darwin

active toolchain
----------------

stable-x86_64-apple-darwin (default)
rustc 1.82.0 (f6e511eec 2024-10-15)
```
This code was developed using rustc `1.68.2` (27/03/2023).

Once installed update rust toolset using:
```bash
rustup update
```

For Rust hints:
```bash
cargo clippy
```


## Source code documentation

To generate source code documentation
```bash
cargo doc
```
This will output documentation to `./target/doc/financing_service_rust/index.html`
[Here ](./../target/doc/financing_service_rust/index.html)



## Directories
The following directories exist in this project:
```
├── data
├── docs
│   └── diagrams
├── src
└── target

```
These directories contain the following:
* `data` - Configuration used by the service
* `docs` - Project documentation
* `docs/diagrams` - PlantUML diagrams and source in support of the documentation
* `src` - Service source code in Rust
* `target` - this is where the Rust compiler places its output