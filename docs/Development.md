# Development

This document contains information useful in installing build tools, building, maintaining and extending this project.

Many of the elements of this service are common with the Financing Service written in Python which can be found under

https://bitbucket.stressedsharks.com/projects/SDL/repos/financing-service

An overview of the project can be found:
https://docs.google.com/document/d/159T_RDgf8CnSq3Kd4PaYgfw9OUrX-kwwdZw4qEe4iP0/edit?usp=sharing


## Rust installation

This project is built using Rust. The best way to install Rust is to use `rustup`.

To determine the current version of Rust run `rustup show`:

```bash
rustup show
```

Once installed, update the Rust toolchain using:

```bash
rustup update
```

## Build and test

```bash
cargo build
cargo test
```

## Formatting and linting

Check formatting:

```bash
cargo fmt --all -- --check
```

Apply formatting:

```bash
cargo fmt --all
```

Run Clippy with warnings denied:

```bash
cargo clippy -- -D warnings
```

These checks run automatically in GitHub Actions (`.github/workflows/rust.yml`) on push and pull request to `main`.

## Source code documentation

Generate source code documentation:

```bash
cargo doc --open
```

This outputs documentation to `./target/doc/financing_service/index.html`.

## Directories

```
├── data
├── docs
│   └── diagrams
├── src
└── target
```

* `data` — configuration used by the service
* `docs` — project documentation
* `docs/diagrams` — PlantUML diagrams and source in support of the documentation
* `src` — service source code in Rust
* `target` — Rust compiler output
