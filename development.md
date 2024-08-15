# Rust version

`rustup default 1.76.0-x86_64-apple-darwin` works. Need <=1.76.0`


# Tests

Tests expect the `DRIFT_GATEWAY_KEY` environment variable to be set, and that
key must have a valid Drift account on devnet.

Devnet fauct:
* https://faucet.devnet.solana.com/
* `solana airdrop 2 <your_pubkey>`

Initialize an account on https://beta.drift.trade, and set your browser wallet
to point to devnet.

Run tests with:
```
cargo test
```
