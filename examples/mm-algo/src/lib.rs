//! Market Making Algo - Tests order placement, fills, rejects, and state tracking.
//!
//! **SAFETY**: By default this algo is OBSERVATION-ONLY (DRY_RUN = true).
//! It will NOT place real orders unless DRY_RUN is set to false.
//!
//! Strategy:
//! - Places bid/ask quotes when spread is wide enough (>5 bps)
//! - Cancels orders when spread tightens or position gets too large
//! - Tracks PnL, fill rates, and market metrics
//!
//! Run: cargo build --target wasm32-unknown-unknown --release

#![cfg_attr(target_arch = "wasm32", no_std)]

#[cfg(target_arch = "wasm32")]
mod wasm {
    extern crate alloc;
    use algo_sdk::*;

    // =========================================================================
    // SAFETY: Set to false to enable LIVE TRADING with real funds!
    // =========================================================================
    const DRY_RUN: bool = true;  // true = observation only, false = LIVE TRADING
    
    // Strategy parameters
    const MIN_SPREAD_BPS: u32 = 5;       // Min spread to quote (5 bps)
    const QUOTE_SIZE_1E8: i64 = 10_000;  // 0.0001 BTC per side (~$8-9)
    const MAX_POSITION_1E8: i64 = 50_000; // 0.0005 BTC max position (~$40-45)
    const EDGE_BPS: u32 = 2;             // Quote edge from mid (2 bps)
    const LOG_EVERY_N: u64 = 100;        // Log market state every N updates
    
    /// Market Making Algo
    struct MmAlgo {
        // Order tracking
        next_order_id: u64,
        bid_order_id: u64,
        ask_order_id: u64,
        
        // Statistics
        update_count: u64,
        quote_count: u64,
        fill_count: u64,
        reject_count: u64,
        cancel_count: u64,
        would_quote_count: u64,  // For dry run - how many times we would have quoted
        
        // Market metrics (rolling)
        sum_spread_bps: u64,
        sum_imbalance: i64,
        min_spread_seen: u32,
        max_spread_seen: u32,
        
        // Price tracking
        last_mid: u64,
        last_bid: u64,
        last_ask: u64,
        price_at_start: u64,
        
        // Book depth tracking
        last_bid_depth: u64,
        last_ask_depth: u64,
    }
    
    impl MmAlgo {
        fn new() -> Self {
            Self {
                next_order_id: 1000,
                bid_order_id: 0,
                ask_order_id: 0,
                update_count: 0,
                quote_count: 0,
                fill_count: 0,
                reject_count: 0,
                cancel_count: 0,
                would_quote_count: 0,
                sum_spread_bps: 0,
                sum_imbalance: 0,
                min_spread_seen: u32::MAX,
                max_spread_seen: 0,
                last_mid: 0,
                last_bid: 0,
                last_ask: 0,
                price_at_start: 0,
                last_bid_depth: 0,
                last_ask_depth: 0,
            }
        }
        
        fn gen_order_id(&mut self) -> u64 {
            self.next_order_id += 1;
            self.next_order_id
        }
        
        /// Calculate quote prices based on mid and edge
        fn quote_prices(&self, mid: u64) -> (u64, u64) {
            let edge = (mid * EDGE_BPS as u64) / 10_000;
            let bid_px = mid.saturating_sub(edge);
            let ask_px = mid + edge;
            (bid_px, ask_px)
        }
        
        /// Check if we should cancel existing orders
        fn should_cancel_quotes(&self, spread_bps: u32, state: &AlgoState) -> bool {
            // Cancel if spread tightened below threshold
            if spread_bps < MIN_SPREAD_BPS {
                return true;
            }
            // Cancel if position too large
            if state.position_1e8.abs() >= MAX_POSITION_1E8 {
                return true;
            }
            false
        }
        
        /// Check if we should place new quotes
        fn should_quote(&self, spread_bps: u32, state: &AlgoState) -> bool {
            // Need wide enough spread
            if spread_bps < MIN_SPREAD_BPS {
                return false;
            }
            // Don't quote if position too large
            if state.position_1e8.abs() >= MAX_POSITION_1E8 {
                return false;
            }
            // Only quote if we don't have orders
            if self.bid_order_id != 0 || self.ask_order_id != 0 {
                return false;
            }
            true
        }
    }
    
    impl Algo for MmAlgo {
        fn on_book(&mut self, book: &L2Book, state: &AlgoState, actions: &mut Actions) {
            self.update_count += 1;
            
            // Extract book data
            let mid = book.mid_px_1e9();
            let spread_bps = book.spread_bps();
            let imbal = book.imbalance_bps(5);
            let bid_depth = book.bid_depth_1e8(5);
            let ask_depth = book.ask_depth_1e8(5);
            
            // Get best bid/ask
            let best_bid = book.best_bid().map(|l| l.px_1e9).unwrap_or(0);
            let best_ask = book.best_ask().map(|l| l.px_1e9).unwrap_or(0);
            
            // Update tracking
            self.sum_spread_bps += spread_bps as u64;
            self.sum_imbalance += imbal as i64;
            self.last_mid = mid;
            self.last_bid = best_bid;
            self.last_ask = best_ask;
            self.last_bid_depth = bid_depth;
            self.last_ask_depth = ask_depth;
            
            // Track spread extremes
            if spread_bps < self.min_spread_seen {
                self.min_spread_seen = spread_bps;
            }
            if spread_bps > self.max_spread_seen && spread_bps < 10000 {
                self.max_spread_seen = spread_bps;
            }
            
            // First update - extensive logging
            if self.update_count == 1 {
                self.price_at_start = mid;
                
                if DRY_RUN {
                    log_info!("=== MM ALGO STARTED (DRY RUN - NO REAL ORDERS) ===");
                } else {
                    log_warn!("=== MM ALGO STARTED (LIVE TRADING ENABLED!) ===");
                }
                
                log_info!(
                    "INIT: mid=${}.{:03} bid=${}.{:03} ask=${}.{:03}",
                    mid / 1_000_000_000, (mid / 1_000_000) % 1000,
                    best_bid / 1_000_000_000, (best_bid / 1_000_000) % 1000,
                    best_ask / 1_000_000_000, (best_ask / 1_000_000) % 1000
                );
                
                log_info!(
                    "INIT: spread={}bps imbal={}bps bid_depth={} ask_depth={}",
                    spread_bps, imbal, bid_depth, ask_depth
                );
                
                log_info!(
                    "PARAMS: min_spread={}bps quote_size={} max_pos={} edge={}bps",
                    MIN_SPREAD_BPS, QUOTE_SIZE_1E8, MAX_POSITION_1E8, EDGE_BPS
                );
                
                // Log full book
                log_info!("BOOK: {} bid levels, {} ask levels", book.bid_ct, book.ask_ct);
                for i in 0..3.min(book.bid_ct as usize) {
                    log_debug!(
                        "  BID[{}]: ${}.{:03} x {}",
                        i,
                        book.bids[i].px_1e9 / 1_000_000_000,
                        (book.bids[i].px_1e9 / 1_000_000) % 1000,
                        book.bids[i].sz_1e8
                    );
                }
                for i in 0..3.min(book.ask_ct as usize) {
                    log_debug!(
                        "  ASK[{}]: ${}.{:03} x {}",
                        i,
                        book.asks[i].px_1e9 / 1_000_000_000,
                        (book.asks[i].px_1e9 / 1_000_000) % 1000,
                        book.asks[i].sz_1e8
                    );
                }
            }
            
            // Periodic market state logging
            if self.update_count % LOG_EVERY_N == 0 {
                let avg_spread = self.sum_spread_bps / self.update_count;
                let price_change_bps = if self.price_at_start > 0 {
                    ((mid as i64 - self.price_at_start as i64) * 10000 / self.price_at_start as i64) as i32
                } else {
                    0
                };
                
                log_info!(
                    "MARKET[{}]: mid=${}.{:03} spread={}bps imbal={}bps price_chg={}bps",
                    self.update_count,
                    mid / 1_000_000_000, (mid / 1_000_000) % 1000,
                    spread_bps, imbal, price_change_bps
                );
                
                log_info!(
                    "STATS[{}]: avg_spread={}bps min={}bps max={}bps would_quote={}",
                    self.update_count,
                    avg_spread, self.min_spread_seen, self.max_spread_seen,
                    self.would_quote_count
                );
                
                log_info!(
                    "STATE[{}]: pos={} pnl={} orders={} fills={} rejects={}",
                    self.update_count,
                    state.position_1e8,
                    state.total_pnl_1e9() / 1_000_000,
                    state.order_ct,
                    self.fill_count,
                    self.reject_count
                );
            }
            
            // Check quoting conditions and log decisions
            let would_quote = self.should_quote(spread_bps, state);
            let would_cancel = self.should_cancel_quotes(spread_bps, state);
            
            // Log decision reasoning periodically
            if self.update_count % (LOG_EVERY_N * 5) == 0 {
                log_debug!(
                    "DECISION: spread_ok={} pos_ok={} no_orders={} -> would_quote={}",
                    spread_bps >= MIN_SPREAD_BPS,
                    state.position_1e8.abs() < MAX_POSITION_1E8,
                    self.bid_order_id == 0 && self.ask_order_id == 0,
                    would_quote
                );
            }
            
            // Cancel logic
            if would_cancel && (self.bid_order_id != 0 || self.ask_order_id != 0) {
                let reason = if spread_bps < MIN_SPREAD_BPS {
                    "spread_tight"
                } else {
                    "position_limit"
                };
                
                log_info!(
                    "CANCEL: reason={} spread={}bps pos={}",
                    reason, spread_bps, state.position_1e8
                );
                
                if !DRY_RUN {
                    if self.bid_order_id != 0 {
                        actions.cancel(self.bid_order_id);
                        self.cancel_count += 1;
                        log_info!("CANCEL_SENT: bid_order={}", self.bid_order_id);
                        self.bid_order_id = 0;
                    }
                    if self.ask_order_id != 0 {
                        actions.cancel(self.ask_order_id);
                        self.cancel_count += 1;
                        log_info!("CANCEL_SENT: ask_order={}", self.ask_order_id);
                        self.ask_order_id = 0;
                    }
                } else {
                    log_debug!("CANCEL_SKIPPED: dry_run=true");
                }
                return;
            }
            
            // Quote logic
            if would_quote {
                self.would_quote_count += 1;
                
                let (bid_px, ask_px) = self.quote_prices(mid);
                
                // Adjust size based on position
                let bid_size = if state.position_1e8 > 0 {
                    QUOTE_SIZE_1E8 / 2
                } else {
                    QUOTE_SIZE_1E8
                };
                
                let ask_size = if state.position_1e8 < 0 {
                    QUOTE_SIZE_1E8 / 2
                } else {
                    QUOTE_SIZE_1E8
                };
                
                log_info!(
                    "QUOTE_SIGNAL: spread={}bps mid=${}.{:03} bid_px=${}.{:03} ask_px=${}.{:03}",
                    spread_bps,
                    mid / 1_000_000_000, (mid / 1_000_000) % 1000,
                    bid_px / 1_000_000_000, (bid_px / 1_000_000) % 1000,
                    ask_px / 1_000_000_000, (ask_px / 1_000_000) % 1000
                );
                
                log_info!(
                    "QUOTE_SIZES: bid_size={} ask_size={} (adjusted for pos={})",
                    bid_size, ask_size, state.position_1e8
                );
                
                if !DRY_RUN {
                    // Place bid
                    self.bid_order_id = self.gen_order_id();
                    actions.buy(self.bid_order_id, bid_size, bid_px);
                    log_warn!(
                        "ORDER_SENT: BUY {} @${}.{:03} order_id={}",
                        bid_size,
                        bid_px / 1_000_000_000, (bid_px / 1_000_000) % 1000,
                        self.bid_order_id
                    );
                    
                    // Place ask
                    self.ask_order_id = self.gen_order_id();
                    actions.sell(self.ask_order_id, ask_size, ask_px);
                    log_warn!(
                        "ORDER_SENT: SELL {} @${}.{:03} order_id={}",
                        ask_size,
                        ask_px / 1_000_000_000, (ask_px / 1_000_000) % 1000,
                        self.ask_order_id
                    );
                    
                    self.quote_count += 2;
                } else {
                    log_info!(
                        "QUOTE_SKIPPED: dry_run=true (would send bid={} ask={})",
                        self.gen_order_id(), self.gen_order_id()
                    );
                }
            }
        }
        
        fn on_fill(&mut self, fill: &Fill, state: &AlgoState) {
            self.fill_count += 1;
            
            let side_str = if fill.side > 0 { "BUY" } else { "SELL" };
            
            log_warn!(
                "=== FILL RECEIVED ==="
            );
            log_warn!(
                "FILL: {} {} @${}.{:03} order_id={}",
                side_str,
                fill.qty_1e8,
                fill.px_1e9 / 1_000_000_000, (fill.px_1e9 / 1_000_000) % 1000,
                fill.order_id
            );
            log_info!(
                "FILL_STATE: new_pos={} realized_pnl={} unrealized_pnl={} total_pnl={}",
                state.position_1e8,
                state.realized_pnl_1e9 / 1_000_000,
                state.unrealized_pnl_1e9 / 1_000_000,
                state.total_pnl_1e9() / 1_000_000
            );
            
            // Clear order ID if it was our quote
            if fill.order_id == self.bid_order_id {
                log_info!("FILL: cleared bid_order_id={}", self.bid_order_id);
                self.bid_order_id = 0;
            }
            if fill.order_id == self.ask_order_id {
                log_info!("FILL: cleared ask_order_id={}", self.ask_order_id);
                self.ask_order_id = 0;
            }
        }
        
        fn on_reject(&mut self, reject: &Reject) {
            self.reject_count += 1;
            
            let reason = match reject.code {
                1 => "RISK",
                2 => "POS_LIMIT",
                3 => "RATE_LIMIT",
                4 => "KILL_SWITCH",
                5 => "INVALID",
                10 => "EXCHANGE",
                _ => "UNKNOWN",
            };
            
            log_error!(
                "=== ORDER REJECTED ==="
            );
            log_error!(
                "REJECT: order_id={} code={} reason={}",
                reject.order_id, reject.code, reason
            );
            
            // Clear order ID if it was our quote
            if reject.order_id == self.bid_order_id {
                log_info!("REJECT: cleared bid_order_id={}", self.bid_order_id);
                self.bid_order_id = 0;
            }
            if reject.order_id == self.ask_order_id {
                log_info!("REJECT: cleared ask_order_id={}", self.ask_order_id);
                self.ask_order_id = 0;
            }
        }
        
        fn on_shutdown(&mut self, state: &AlgoState, actions: &mut Actions) {
            log_warn!("=== MM ALGO SHUTTING DOWN ===");
            
            log_info!(
                "FINAL_STATS: updates={} quotes={} fills={} rejects={} cancels={}",
                self.update_count,
                self.quote_count,
                self.fill_count,
                self.reject_count,
                self.cancel_count
            );
            
            log_info!(
                "FINAL_MARKET: min_spread={}bps max_spread={}bps would_quote={}",
                self.min_spread_seen,
                self.max_spread_seen,
                self.would_quote_count
            );
            
            log_info!(
                "FINAL_POSITION: pos={} pnl={}",
                state.position_1e8,
                state.total_pnl_1e9() / 1_000_000
            );
            
            if DRY_RUN {
                log_info!("MODE: DRY_RUN (no real orders were placed)");
            } else {
                log_warn!("MODE: LIVE (real orders were placed!)");
                // Cancel all open orders
                log_info!("SHUTDOWN: cancelling {} open orders", state.order_ct);
                actions.cancel_all(state);
            }
        }
    }
    
    export_algo!(MmAlgo::new());
    
    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo) -> ! {
        loop {}
    }
    
    #[global_allocator]
    static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;
}

// Empty module for native builds
#[cfg(not(target_arch = "wasm32"))]
mod native {}
