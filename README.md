# sequence-algo-sdk

[![Crates.io](https://img.shields.io/crates/v/sequence-algo-sdk.svg)](https://crates.io/crates/sequence-algo-sdk)
[![docs.rs](https://docs.rs/sequence-algo-sdk/badge.svg)](https://docs.rs/sequence-algo-sdk)
[![License](https://img.shields.io/crates/l/sequence-algo-sdk.svg)](LICENSE-MIT)

Write ultra-low-latency trading algos in Rust, compile to WASM, deploy to [Sequence Markets](https://sequencemkts.com). Zero dependencies. The SDK provides the `Algo` trait, order book types, and action buffers — everything you need to write a trading algorithm that runs on Sequence's edge infrastructure.

## Quick Start

```toml
# Cargo.toml
[package]
name = "my-algo"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
sequence-algo-sdk = { version = "0.1", default-features = false }

[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
panic = "abort"
```

```rust
// src/lib.rs
#![no_std]
extern crate alloc;
use algo_sdk::*;

struct MyAlgo {
    next_id: u64,
}

impl Algo for MyAlgo {
    fn on_book(&mut self, book: &L2Book, state: &AlgoState, actions: &mut Actions) {
        // Your trading logic here
        if book.spread_bps() > 10 && state.is_flat() {
            self.next_id += 1;
            actions.buy(self.next_id, 1_000_000, book.bids[0].px_1e9 + 100);
        }
    }

    fn on_fill(&mut self, _fill: &Fill, _state: &AlgoState) {}
    fn on_reject(&mut self, _reject: &Reject) {}
    fn on_shutdown(&mut self, _state: &AlgoState, actions: &mut Actions) {
        actions.clear(); // Cancel all pending orders
    }
}

export_algo!(MyAlgo { next_id: 0 });

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! { loop {} }

```

## Build & Deploy

Install the [Sequence CLI](https://github.com/Bai-Funds/algo-sdk/releases) and deploy:

```bash
sequence build
sequence deploy BTC-USD --start
sequence logs BTC-USD --follow
```

Or build manually:

```bash
cargo build --target wasm32-unknown-unknown --release
```

## Key Types

| Type | Description |
|------|-------------|
| `Algo` | Trait your algo implements — `on_book`, `on_fill`, `on_reject`, `on_shutdown` |
| `L2Book` | 20-level order book (688 bytes, fits in L1 cache) |
| `AlgoState` | Position + open orders, server-managed |
| `Actions` | Buffer for placing/canceling orders (up to 16 per tick) |
| `Fill` | Fill event with price, quantity, and timing |
| `Reject` | Rejection with typed error codes |

## Fixed-Point Format

All prices and quantities use fixed-point integers for deterministic, allocation-free arithmetic:
- **Prices:** `px_1e9` — multiply by 10^9 (e.g., $50,000.00 = `50_000_000_000_000`)
- **Quantities:** `qty_1e8` — multiply by 10^8 (e.g., 1.0 BTC = `100_000_000`)

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
