//! Speed Test Algo - Measures REAL network latency with live order round-trips.
//!
//! Strategy:
//! - Place aggressive buy (at ask) to get immediate fill
//! - On fill, immediately place aggressive sell (at bid)
//! - Track REAL wall-clock timestamps for accurate latency measurement

#![cfg_attr(target_arch = "wasm32", no_std)]

#[cfg(target_arch = "wasm32")]
mod wasm {
    extern crate alloc;
    use algo_sdk::*;

    // =========================================================================
    // CONFIGURATION
    // =========================================================================
    
    const MAX_ROUND_TRIPS: u64 = 5;
    const ORDER_SIZE_1E8: i64 = 220_000_000;  // 2.2 XRP (~$4)
    const MAX_SPREAD_BPS: u32 = 50;
    const MIN_UPDATES_BETWEEN_ORDERS: u64 = 1;  // Fire ASAP
    const LOG_INTERVAL: u64 = 50;

    // =========================================================================
    // STATE
    // =========================================================================
    
    #[derive(Clone, Copy, PartialEq)]
    enum Phase {
        Idle,
        BuyPending,
        ReadyToSell,
        SellPending,
        Complete,
    }
    
    struct SpeedTestAlgo {
        next_order_id: u64,
        current_order_id: u64,
        phase: Phase,
        round_trip_count: u64,
        
        // Wall clock timestamps (nanoseconds since epoch)
        order_sent_ns: u64,
        round_trip_start_ns: u64,
        
        // Latency stats
        buy_latencies_ms: [u64; 16],
        sell_latencies_ms: [u64; 16],
        round_trip_latencies_ms: [u64; 16],
        total_buy_latency_ms: u64,
        total_sell_latency_ms: u64,
        total_round_trip_ms: u64,
        min_latency_ms: u64,
        max_latency_ms: u64,
        
        // Counters
        update_count: u64,
        buy_fill_count: u64,
        sell_fill_count: u64,
        last_order_update: u64,
    }
    
    impl SpeedTestAlgo {
        fn new() -> Self {
            Self {
                next_order_id: 5000,
                current_order_id: 0,
                phase: Phase::Idle,
                round_trip_count: 0,
                order_sent_ns: 0,
                round_trip_start_ns: 0,
                buy_latencies_ms: [0; 16],
                sell_latencies_ms: [0; 16],
                round_trip_latencies_ms: [0; 16],
                total_buy_latency_ms: 0,
                total_sell_latency_ms: 0,
                total_round_trip_ms: 0,
                min_latency_ms: u64::MAX,
                max_latency_ms: 0,
                update_count: 0,
                buy_fill_count: 0,
                sell_fill_count: 0,
                last_order_update: 0,
            }
        }
        
        fn gen_order_id(&mut self) -> u64 {
            self.next_order_id += 1;
            self.next_order_id
        }
        
        fn record_latency(&mut self, latency_ms: u64) {
            if latency_ms < self.min_latency_ms && latency_ms > 0 {
                self.min_latency_ms = latency_ms;
            }
            if latency_ms > self.max_latency_ms {
                self.max_latency_ms = latency_ms;
            }
        }
    }
    
    impl Algo for SpeedTestAlgo {
        fn on_book(&mut self, book: &L2Book, _state: &AlgoState, actions: &mut Actions) {
            self.update_count += 1;
            
            let spread_bps = book.spread_bps();
            let best_bid = book.best_bid().map(|l| l.px_1e9).unwrap_or(0);
            let best_ask = book.best_ask().map(|l| l.px_1e9).unwrap_or(0);
            
            if self.update_count == 1 {
                log_warn!("=== SPEED TEST ALGO (LIVE) ===");
                log_info!("CONFIG: trips={} size={} XRP", MAX_ROUND_TRIPS, ORDER_SIZE_1E8 / 100_000_000);
                log_info!("INIT: bid=${}.{:03} ask=${}.{:03} spread={}bps", 
                    best_bid / 1_000_000_000, (best_bid / 1_000_000) % 1000,
                    best_ask / 1_000_000_000, (best_ask / 1_000_000) % 1000,
                    spread_bps);
            }
            
            // Periodic stats
            if self.update_count % LOG_INTERVAL == 0 && self.round_trip_count > 0 {
                let avg_rt = self.total_round_trip_ms / self.round_trip_count.max(1);
                log_info!(
                    "STATS[{}]: trips={} avg_rt={}ms min={}ms max={}ms",
                    self.update_count, self.round_trip_count, avg_rt,
                    if self.min_latency_ms == u64::MAX { 0 } else { self.min_latency_ms },
                    self.max_latency_ms
                );
            }
            
            if self.phase == Phase::Complete {
                return;
            }
            
            // Cooldown
            if self.update_count - self.last_order_update < MIN_UPDATES_BETWEEN_ORDERS {
                return;
            }
            
            // Spread check
            if spread_bps > MAX_SPREAD_BPS {
                return;
            }
            
            match self.phase {
                Phase::Idle => {
                    if self.round_trip_count >= MAX_ROUND_TRIPS {
                        self.phase = Phase::Complete;
                        self.log_final_stats();
                        return;
                    }
                    
                    // Start round trip with MARKET BUY (wall clock timestamp)
                    self.round_trip_start_ns = book.recv_ns;
                    self.order_sent_ns = book.recv_ns;
                    self.current_order_id = self.gen_order_id();
                    self.last_order_update = self.update_count;
                    
                    // Use MARKET order for immediate execution
                    actions.market_buy(self.current_order_id, ORDER_SIZE_1E8);
                    self.phase = Phase::BuyPending;
                    
                    log_info!(
                        "MARKET BUY: trip={} order={} qty={}",
                        self.round_trip_count + 1, self.current_order_id,
                        ORDER_SIZE_1E8 / 100_000_000
                    );
                }
                
                Phase::BuyPending => {
                    // Waiting for buy fill
                }
                
                Phase::ReadyToSell => {
                    // Send MARKET SELL (wall clock timestamp)
                    self.order_sent_ns = book.recv_ns;
                    self.current_order_id = self.gen_order_id();
                    self.last_order_update = self.update_count;
                    
                    // Use MARKET order for immediate execution
                    actions.market_sell(self.current_order_id, ORDER_SIZE_1E8);
                    self.phase = Phase::SellPending;
                    
                    log_info!(
                        "MARKET SELL: trip={} order={} qty={}",
                        self.round_trip_count + 1, self.current_order_id,
                        ORDER_SIZE_1E8 / 100_000_000
                    );
                }
                
                Phase::SellPending => {
                    // Waiting for sell fill
                }
                
                Phase::Complete => {}
            }
        }
        
        fn on_fill(&mut self, fill: &Fill, _state: &AlgoState) {
            // Calculate latency using wall clock (fill.recv_ns and order_sent_ns both wall time)
            let latency_ns = fill.recv_ns.saturating_sub(self.order_sent_ns);
            let latency_ms = latency_ns / 1_000_000;
            
            self.record_latency(latency_ms);
            
            if fill.side > 0 {
                // BUY filled
                let idx = (self.buy_fill_count as usize) % 16;
                self.buy_latencies_ms[idx] = latency_ms;
                self.buy_fill_count += 1;
                self.total_buy_latency_ms += latency_ms;
                
                log_warn!(
                    "BUY FILL: order={} qty={} @${}.{:03} latency={}ms",
                    fill.order_id, fill.qty_1e8,
                    fill.px_1e9 / 1_000_000_000, (fill.px_1e9 / 1_000_000) % 1000,
                    latency_ms
                );
                
                // Ready to sell
                self.phase = Phase::ReadyToSell;
            } else {
                // SELL filled
                let idx = (self.sell_fill_count as usize) % 16;
                self.sell_latencies_ms[idx] = latency_ms;
                self.sell_fill_count += 1;
                self.total_sell_latency_ms += latency_ms;
                
                // Full round trip complete (using wall time)
                let rt_ns = fill.recv_ns.saturating_sub(self.round_trip_start_ns);
                let rt_ms = rt_ns / 1_000_000;
                let rt_idx = (self.round_trip_count as usize) % 16;
                self.round_trip_latencies_ms[rt_idx] = rt_ms;
                self.total_round_trip_ms += rt_ms;
                self.round_trip_count += 1;
                
                log_warn!(
                    "=== ROUND TRIP #{} COMPLETE: {}ms (buy={}ms sell={}ms) ===",
                    self.round_trip_count, rt_ms,
                    self.buy_latencies_ms[(self.buy_fill_count as usize - 1) % 16],
                    latency_ms
                );
                
                if self.round_trip_count >= MAX_ROUND_TRIPS {
                    self.phase = Phase::Complete;
                    self.log_final_stats();
                } else {
                    self.phase = Phase::Idle;
                }
            }
        }
        
        fn on_reject(&mut self, reject: &Reject) {
            log_error!("REJECT: order={} code={}", reject.order_id, reject.code);
            // Reset to idle to try again
            self.phase = Phase::Idle;
        }
        
        fn on_shutdown(&mut self, _state: &AlgoState, _actions: &mut Actions) {
            log_warn!("=== SHUTDOWN ===");
            self.log_final_stats();
        }
    }
    
    impl SpeedTestAlgo {
        fn log_final_stats(&self) {
            log_warn!("==========================================");
            log_warn!("=== FINAL LATENCY RESULTS ===");
            log_warn!("==========================================");
            
            log_info!("TRIPS COMPLETED: {}/{}", self.round_trip_count, MAX_ROUND_TRIPS);
            
            if self.round_trip_count == 0 {
                log_warn!("No completed round trips");
                return;
            }
            
            let avg_buy = self.total_buy_latency_ms / self.buy_fill_count.max(1);
            let avg_sell = self.total_sell_latency_ms / self.sell_fill_count.max(1);
            let avg_rt = self.total_round_trip_ms / self.round_trip_count.max(1);
            
            log_warn!("--- AVERAGE LATENCY (ms) ---");
            log_info!("BUY:        {}ms", avg_buy);
            log_info!("SELL:       {}ms", avg_sell);
            log_info!("ROUND TRIP: {}ms", avg_rt);
            log_info!("MIN/MAX:    {}ms / {}ms",
                if self.min_latency_ms == u64::MAX { 0 } else { self.min_latency_ms },
                self.max_latency_ms);
            
            log_warn!("--- INDIVIDUAL ROUND TRIPS ---");
            for i in 0..self.round_trip_count.min(16) as usize {
                log_info!(
                    "  RT#{}: {}ms total (buy={}ms sell={}ms)",
                    i + 1,
                    self.round_trip_latencies_ms[i],
                    self.buy_latencies_ms[i],
                    self.sell_latencies_ms[i]
                );
            }
            
            log_warn!("==========================================");
        }
    }
    
    export_algo!(SpeedTestAlgo::new());
    
    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo) -> ! {
        loop {}
    }
    
}

#[cfg(not(target_arch = "wasm32"))]
mod native {}
