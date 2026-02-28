//! Test Algo - Validates SDK and WASM execution pipeline.
//!
//! This crate only compiles for wasm32-unknown-unknown target.
//! Run: cargo build --target wasm32-unknown-unknown --release

#![cfg_attr(target_arch = "wasm32", no_std)]

#[cfg(target_arch = "wasm32")]
mod wasm {
    extern crate alloc;
    use algo_sdk::*;

    /// Simple test algo that logs book updates.
    struct TestAlgo {
        update_count: u64,
        last_mid: u64,
    }

    impl Algo for TestAlgo {
        fn on_book(&mut self, book: &L2Book, state: &AlgoState, _actions: &mut Actions) {
            self.update_count += 1;

            if self.update_count == 1 {
                log::info("TestAlgo: First book update received!");
            }

            if self.update_count % 1000 == 0 {
                let mid = book.mid_px_1e9();
                let spread_bps = book.spread_bps();
                let imbal = book.imbalance_bps(5);
                
                log_info!(
                    "updates={} mid={} spread={}bps imbal={}bps pos={}",
                    self.update_count,
                    mid / 1_000_000,
                    spread_bps,
                    imbal,
                    state.position_1e8
                );
                
                self.last_mid = mid;
            }
        }

        fn on_fill(&mut self, fill: &Fill, _state: &AlgoState) {
            log_info!(
                "FILL: order={} side={} qty={} px={}",
                fill.order_id,
                fill.side,
                fill.qty_1e8,
                fill.px_1e9
            );
        }

        fn on_reject(&mut self, reject: &Reject) {
            log_warn!(
                "REJECT: order={} code={}",
                reject.order_id,
                reject.code
            );
        }

        fn on_shutdown(&mut self, _state: &AlgoState, actions: &mut Actions) {
            log_info!("TestAlgo shutting down after {} updates", self.update_count);
            actions.clear();
        }
    }

    export_algo!(TestAlgo {
        update_count: 0,
        last_mid: 0,
    });

    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo) -> ! {
        loop {}
    }

    #[global_allocator]
    static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;
}

// Empty module for native builds (allows cargo check to pass)
#[cfg(not(target_arch = "wasm32"))]
mod native {
    // This crate is WASM-only. Build with:
    // cargo build --target wasm32-unknown-unknown --release
}
