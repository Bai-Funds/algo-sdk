//! Sequence Algo SDK - Ultra Low Latency Trading
//!
//! Write HFT algorithms in Rust, compile to WASM, deploy to Sequence.
//!
//! # Example
//! ```rust,ignore
//! use algo_sdk::*;
//!
//! struct MyAlgo { next_id: u64 }
//!
//! impl Algo for MyAlgo {
//!     fn on_book(&mut self, book: &L2Book, state: &AlgoState, actions: &mut Actions) {
//!         if book.spread_bps() > 10 && state.position_1e8.abs() < 100_000_000 {
//!             self.next_id += 1;
//!             actions.buy(self.next_id, 1_000_000, book.bids[0].px_1e9 + 100);
//!         }
//!     }
//!     fn on_fill(&mut self, _: &Fill, _: &AlgoState) {}
//!     fn on_reject(&mut self, _: &Reject) {}
//!     fn on_shutdown(&mut self, _: &AlgoState, _: &mut Actions) {}
//! }
//!
//! export_algo!(MyAlgo { next_id: 0 });
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![allow(non_snake_case)]

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(feature = "std")]
extern crate std as alloc;

// =============================================================================
// PRICE LEVEL
// =============================================================================

/// Single price level in the order book.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct Level {
    pub px_1e9: u64, // Price × 10⁹
    pub sz_1e8: u64, // Size × 10⁸
}

impl Level {
    pub const EMPTY: Self = Self {
        px_1e9: 0,
        sz_1e8: 0,
    };

    #[inline(always)]
    pub fn is_valid(&self) -> bool {
        self.px_1e9 > 0
    }
}

// =============================================================================
// L2 ORDER BOOK - 20 levels each side
// =============================================================================

/// L2 order book with up to 20 levels per side.
/// Total size: 688 bytes (fits in L1 cache).
#[derive(Clone, Copy)]
#[repr(C)]
pub struct L2Book {
    pub bids: [Level; 20], // Best (index 0) to worst
    pub asks: [Level; 20], // Best (index 0) to worst
    pub bid_ct: u8,        // Valid bid levels
    pub ask_ct: u8,        // Valid ask levels
    pub symbol_id: u16,
    pub _pad: u32,
    pub recv_ns: u64, // Receive timestamp
}

impl Default for L2Book {
    fn default() -> Self {
        Self {
            bids: [Level::EMPTY; 20],
            asks: [Level::EMPTY; 20],
            bid_ct: 0,
            ask_ct: 0,
            symbol_id: 0,
            _pad: 0,
            recv_ns: 0,
        }
    }
}

impl L2Book {
    #[inline(always)]
    pub fn best_bid(&self) -> Option<&Level> {
        if self.bid_ct > 0 && self.bids[0].px_1e9 > 0 {
            Some(&self.bids[0])
        } else {
            None
        }
    }

    #[inline(always)]
    pub fn best_ask(&self) -> Option<&Level> {
        if self.ask_ct > 0 && self.asks[0].px_1e9 > 0 {
            Some(&self.asks[0])
        } else {
            None
        }
    }

    #[inline(always)]
    pub fn mid_px_1e9(&self) -> u64 {
        if self.bid_ct == 0 || self.ask_ct == 0 {
            return 0;
        }
        (self.bids[0].px_1e9 + self.asks[0].px_1e9) / 2
    }

    #[inline(always)]
    pub fn spread_1e9(&self) -> u64 {
        if self.bid_ct == 0 || self.ask_ct == 0 {
            return u64::MAX;
        }
        self.asks[0].px_1e9.saturating_sub(self.bids[0].px_1e9)
    }

    #[inline(always)]
    pub fn spread_bps(&self) -> u32 {
        let mid = self.mid_px_1e9();
        if mid == 0 {
            return u32::MAX;
        }
        ((self.spread_1e9() * 10_000) / mid) as u32
    }

    #[inline(always)]
    pub fn bid_depth_1e8(&self, levels: usize) -> u64 {
        let n = levels.min(self.bid_ct as usize);
        let mut sum = 0u64;
        for i in 0..n {
            sum += self.bids[i].sz_1e8;
        }
        sum
    }

    #[inline(always)]
    pub fn ask_depth_1e8(&self, levels: usize) -> u64 {
        let n = levels.min(self.ask_ct as usize);
        let mut sum = 0u64;
        for i in 0..n {
            sum += self.asks[i].sz_1e8;
        }
        sum
    }

    #[inline(always)]
    pub fn imbalance_bps(&self, levels: usize) -> i32 {
        let bid_depth = self.bid_depth_1e8(levels);
        let ask_depth = self.ask_depth_1e8(levels);
        let total = bid_depth + ask_depth;
        if total == 0 {
            return 0;
        }
        (((bid_depth as i64 - ask_depth as i64) * 10_000) / total as i64) as i32
    }
}

// =============================================================================
// OPEN ORDER
// =============================================================================

/// Order status.
pub mod Status {
    pub const PENDING: u8 = 0; // Sent, awaiting ack
    pub const ACKED: u8 = 1; // Acknowledged by exchange
    pub const PARTIAL: u8 = 2; // Partially filled
    pub const DEAD: u8 = 3; // Filled/cancelled/rejected
}

/// Open order tracked by server.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct OpenOrder {
    pub order_id: u64,
    pub px_1e9: u64,
    pub qty_1e8: i64,    // Signed: positive=buy, negative=sell
    pub filled_1e8: i64, // Amount filled
    pub side: i8,        // 1=buy, -1=sell
    pub status: u8,
    pub _pad: [u8; 6],
}

impl OpenOrder {
    pub const EMPTY: Self = Self {
        order_id: 0,
        px_1e9: 0,
        qty_1e8: 0,
        filled_1e8: 0,
        side: 0,
        status: 0,
        _pad: [0; 6],
    };

    #[inline(always)]
    pub fn is_live(&self) -> bool {
        self.status == Status::ACKED || self.status == Status::PARTIAL
    }

    #[inline(always)]
    pub fn is_pending(&self) -> bool {
        self.status == Status::PENDING
    }

    #[inline(always)]
    pub fn remaining_1e8(&self) -> i64 {
        self.qty_1e8.abs() - self.filled_1e8.abs()
    }
}

// =============================================================================
// ALGO STATE - Position + Orders (server-managed)
// =============================================================================

/// Maximum open orders per algo.
pub const MAX_ORDERS: usize = 32;

/// Algo state: position and open orders, managed by server.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct AlgoState {
    // Position
    pub position_1e8: i64,       // Net position (positive=long)
    pub avg_entry_1e9: u64,      // Average entry price
    pub realized_pnl_1e9: i64,   // Realized PnL
    pub unrealized_pnl_1e9: i64, // Unrealized PnL
    // Orders
    pub orders: [OpenOrder; MAX_ORDERS],
    pub order_ct: u8,
    pub _pad: [u8; 7],
}

impl Default for AlgoState {
    fn default() -> Self {
        Self {
            position_1e8: 0,
            avg_entry_1e9: 0,
            realized_pnl_1e9: 0,
            unrealized_pnl_1e9: 0,
            orders: [OpenOrder::EMPTY; MAX_ORDERS],
            order_ct: 0,
            _pad: [0; 7],
        }
    }
}

/// PnL snapshot in fixed-point units (1e9 = $1.00).
#[derive(Debug, Clone, Copy, Default)]
pub struct PnlSnapshot {
    pub realized_1e9: i64,
    pub unrealized_1e9: i64,
    pub total_1e9: i64,
}

impl AlgoState {
    #[inline(always)]
    pub fn is_flat(&self) -> bool {
        self.position_1e8 == 0
    }

    #[inline(always)]
    pub fn is_long(&self) -> bool {
        self.position_1e8 > 0
    }

    #[inline(always)]
    pub fn is_short(&self) -> bool {
        self.position_1e8 < 0
    }

    #[inline(always)]
    pub fn has_orders(&self) -> bool {
        self.order_ct > 0
    }

    #[inline(always)]
    pub fn live_order_count(&self) -> usize {
        let mut ct = 0;
        for i in 0..self.order_ct as usize {
            if self.orders[i].is_live() {
                ct += 1;
            }
        }
        ct
    }

    #[inline(always)]
    pub fn find_order(&self, order_id: u64) -> Option<&OpenOrder> {
        for i in 0..self.order_ct as usize {
            if self.orders[i].order_id == order_id {
                return Some(&self.orders[i]);
            }
        }
        None
    }

    #[inline(always)]
    pub fn open_buy_qty_1e8(&self) -> i64 {
        let mut sum = 0i64;
        for i in 0..self.order_ct as usize {
            let o = &self.orders[i];
            if o.is_live() && o.side > 0 {
                sum += o.remaining_1e8();
            }
        }
        sum
    }

    #[inline(always)]
    pub fn open_sell_qty_1e8(&self) -> i64 {
        let mut sum = 0i64;
        for i in 0..self.order_ct as usize {
            let o = &self.orders[i];
            if o.is_live() && o.side < 0 {
                sum += o.remaining_1e8();
            }
        }
        sum
    }

    #[inline(always)]
    pub fn total_pnl_1e9(&self) -> i64 {
        self.realized_pnl_1e9 + self.unrealized_pnl_1e9
    }

    /// Ergonomic PnL accessor for strategies that want a single call.
    #[inline(always)]
    pub fn get_pnl(&self) -> PnlSnapshot {
        PnlSnapshot {
            realized_1e9: self.realized_pnl_1e9,
            unrealized_1e9: self.unrealized_pnl_1e9,
            total_1e9: self.total_pnl_1e9(),
        }
    }

    /// Realized PnL as USD float.
    #[inline(always)]
    pub fn realized_pnl_usd(&self) -> f64 {
        self.realized_pnl_1e9 as f64 / 1e9
    }

    /// Unrealized PnL as USD float.
    #[inline(always)]
    pub fn unrealized_pnl_usd(&self) -> f64 {
        self.unrealized_pnl_1e9 as f64 / 1e9
    }

    /// Total PnL as USD float.
    #[inline(always)]
    pub fn total_pnl_usd(&self) -> f64 {
        self.total_pnl_1e9() as f64 / 1e9
    }
}

// =============================================================================
// FILL EVENT
// =============================================================================

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Fill {
    pub order_id: u64,
    pub px_1e9: u64,
    pub qty_1e8: i64,
    pub recv_ns: u64, // Timestamp when fill was received
    pub side: i8,
    pub _pad: [u8; 7],
}

impl Fill {
    /// Elapsed time since a caller-provided start timestamp.
    /// Typical use: `fill.since_ms(start_ns)` where `start_ns` came from `book.recv_ns`.
    #[inline(always)]
    pub fn since_ns(&self, start_ns: u64) -> u64 {
        self.recv_ns.saturating_sub(start_ns)
    }

    #[inline(always)]
    pub fn since_us(&self, start_ns: u64) -> u64 {
        self.since_ns(start_ns) / 1_000
    }

    #[inline(always)]
    pub fn since_ms(&self, start_ns: u64) -> u64 {
        self.since_ns(start_ns) / 1_000_000
    }
}

// =============================================================================
// TIME HELPERS
// =============================================================================

/// Simple timing helpers for client-controlled latency measurement.
/// SDK/runtime does not force any logging; strategy decides how/when to log.
pub mod time {
    /// Start a timer from an event timestamp (usually `book.recv_ns` or `fill.recv_ns`).
    #[inline(always)]
    pub fn start(now_ns: u64) -> u64 {
        now_ns
    }

    /// Stop a timer and return elapsed nanoseconds.
    #[inline(always)]
    pub fn stop_ns(start_ns: u64, now_ns: u64) -> u64 {
        now_ns.saturating_sub(start_ns)
    }

    #[inline(always)]
    pub fn stop_us(start_ns: u64, now_ns: u64) -> u64 {
        stop_ns(start_ns, now_ns) / 1_000
    }

    #[inline(always)]
    pub fn stop_ms(start_ns: u64, now_ns: u64) -> u64 {
        stop_ns(start_ns, now_ns) / 1_000_000
    }

    /// Stateful timer for strategies that prefer start/stop on a struct.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct Timer {
        start_ns: u64,
    }

    impl Timer {
        #[inline(always)]
        pub const fn new() -> Self {
            Self { start_ns: 0 }
        }

        #[inline(always)]
        pub fn start(&mut self, now_ns: u64) {
            self.start_ns = now_ns;
        }

        #[inline(always)]
        pub fn stop_ns(&self, now_ns: u64) -> u64 {
            now_ns.saturating_sub(self.start_ns)
        }

        #[inline(always)]
        pub fn stop_us(&self, now_ns: u64) -> u64 {
            self.stop_ns(now_ns) / 1_000
        }

        #[inline(always)]
        pub fn stop_ms(&self, now_ns: u64) -> u64 {
            self.stop_ns(now_ns) / 1_000_000
        }
    }
}

// =============================================================================
// REJECT EVENT
// =============================================================================

/// Reject codes from exchange (matches cc_proto::RejectClass).
pub mod RejectCode {
    /// Unknown error
    pub const UNKNOWN: u8 = 0;
    /// Insufficient balance/funds
    pub const INSUFFICIENT_BALANCE: u8 = 1;
    /// Invalid parameters (price, qty, symbol)
    pub const INVALID_PARAMS: u8 = 2;
    /// Exchange rate limit hit
    pub const RATE_LIMIT: u8 = 3;
    /// Exchange temporarily unavailable
    pub const EXCHANGE_BUSY: u8 = 4;
    /// Network error
    pub const NETWORK: u8 = 5;
    /// Authentication error (invalid API key/secret)
    pub const AUTH: u8 = 6;

    // Internal reject codes (from risk engine, >=100)
    /// Risk check failed
    pub const RISK: u8 = 100;
    /// Position limit exceeded
    pub const POSITION_LIMIT: u8 = 101;
    /// Kill switch triggered
    pub const KILL_SWITCH: u8 = 102;
    /// Fat-finger: price deviates too far from reference (best bid/ask)
    pub const PRICE_DEVIATION: u8 = 103;
    /// Daily P&L loss limit breached — algo paused
    pub const DAILY_LOSS_LIMIT: u8 = 104;

    /// Get human-readable description for a reject code.
    pub fn to_str(code: u8) -> &'static str {
        match code {
            UNKNOWN => "UNKNOWN",
            INSUFFICIENT_BALANCE => "INSUFFICIENT_BALANCE",
            INVALID_PARAMS => "INVALID_PARAMS",
            RATE_LIMIT => "RATE_LIMIT",
            EXCHANGE_BUSY => "EXCHANGE_BUSY",
            NETWORK => "NETWORK",
            AUTH => "AUTH",
            RISK => "RISK_CHECK_FAILED",
            POSITION_LIMIT => "POSITION_LIMIT",
            KILL_SWITCH => "KILL_SWITCH",
            PRICE_DEVIATION => "PRICE_DEVIATION",
            DAILY_LOSS_LIMIT => "DAILY_LOSS_LIMIT",
            _ => "UNKNOWN",
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Reject {
    pub order_id: u64,
    pub code: u8,
    pub _pad: [u8; 7],
}

impl Reject {
    /// Get human-readable description of the reject reason.
    #[inline]
    pub fn reason(&self) -> &'static str {
        RejectCode::to_str(self.code)
    }
}

// =============================================================================
// ORDER TYPES
// =============================================================================

/// Order type for execution semantics.
pub mod OrderType {
    /// Limit order - sits on book until filled or canceled (default).
    pub const LIMIT: u8 = 0;
    /// Market order - fills immediately at best available price.
    pub const MARKET: u8 = 1;
    /// Immediate-Or-Cancel - fill what you can, cancel the rest.
    pub const IOC: u8 = 2;
    /// Fill-Or-Kill - fill entire qty or reject completely.
    pub const FOK: u8 = 3;
    /// Post-only - sits on book, rejected if it would cross (maker only).
    /// Maps to venue-native post-only (Kraken `post_only:true`, Binance `LIMIT_MAKER`).
    /// Venues without native post-only reject with `RejectCode::INVALID_PARAMS`.
    pub const POST_ONLY: u8 = 4;
}

// =============================================================================
// ACTIONS BUFFER
// =============================================================================

/// Order action (place or cancel).
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct Action {
    pub order_id: u64,
    pub px_1e9: u64,
    pub qty_1e8: i64,
    pub side: i8, // 1=buy, -1=sell, 0=cancel
    pub is_cancel: u8,
    pub order_type: u8, // OrderType::LIMIT, MARKET, IOC, FOK
    pub _pad: [u8; 5],
}

/// Max actions per callback.
pub const MAX_ACTIONS: usize = 16;

/// Actions buffer - orders to send.
#[repr(C)]
pub struct Actions {
    actions: [Action; MAX_ACTIONS],
    len: usize,
}

impl Actions {
    #[inline(always)]
    pub const fn new() -> Self {
        Self {
            actions: [Action {
                order_id: 0,
                px_1e9: 0,
                qty_1e8: 0,
                side: 0,
                is_cancel: 0,
                order_type: 0,
                _pad: [0; 5],
            }; MAX_ACTIONS],
            len: 0,
        }
    }

    #[inline(always)]
    pub fn clear(&mut self) {
        self.len = 0;
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline(always)]
    pub fn is_full(&self) -> bool {
        self.len >= MAX_ACTIONS
    }

    // =========================================================================
    // LIMIT ORDERS (default) - sit on book until filled or canceled
    // =========================================================================

    /// Place a limit buy order (GTC - Good Till Canceled).
    #[inline(always)]
    pub fn buy(&mut self, order_id: u64, qty_1e8: i64, px_1e9: u64) -> bool {
        self.order_typed(order_id, 1, qty_1e8, px_1e9, OrderType::LIMIT)
    }

    /// Place a limit sell order (GTC - Good Till Canceled).
    #[inline(always)]
    pub fn sell(&mut self, order_id: u64, qty_1e8: i64, px_1e9: u64) -> bool {
        self.order_typed(order_id, -1, qty_1e8, px_1e9, OrderType::LIMIT)
    }

    /// Place limit order with explicit side (1=buy, -1=sell).
    #[inline(always)]
    pub fn order(&mut self, order_id: u64, side: i8, qty_1e8: i64, px_1e9: u64) -> bool {
        self.order_typed(order_id, side, qty_1e8, px_1e9, OrderType::LIMIT)
    }

    // =========================================================================
    // MARKET ORDERS - fill immediately at best available price
    // =========================================================================

    /// Place a market buy order (fills immediately).
    #[inline(always)]
    pub fn market_buy(&mut self, order_id: u64, qty_1e8: i64) -> bool {
        self.order_typed(order_id, 1, qty_1e8, 0, OrderType::MARKET)
    }

    /// Place a market sell order (fills immediately).
    #[inline(always)]
    pub fn market_sell(&mut self, order_id: u64, qty_1e8: i64) -> bool {
        self.order_typed(order_id, -1, qty_1e8, 0, OrderType::MARKET)
    }

    // =========================================================================
    // IOC ORDERS - Immediate-Or-Cancel (fill what you can, cancel rest)
    // =========================================================================

    /// Place IOC buy - fills available liquidity, cancels unfilled portion.
    #[inline(always)]
    pub fn ioc_buy(&mut self, order_id: u64, qty_1e8: i64, px_1e9: u64) -> bool {
        self.order_typed(order_id, 1, qty_1e8, px_1e9, OrderType::IOC)
    }

    /// Place IOC sell - fills available liquidity, cancels unfilled portion.
    #[inline(always)]
    pub fn ioc_sell(&mut self, order_id: u64, qty_1e8: i64, px_1e9: u64) -> bool {
        self.order_typed(order_id, -1, qty_1e8, px_1e9, OrderType::IOC)
    }

    // =========================================================================
    // FOK ORDERS - Fill-Or-Kill (fill entire qty or reject)
    // =========================================================================

    /// Place FOK buy - must fill entire quantity or rejected.
    #[inline(always)]
    pub fn fok_buy(&mut self, order_id: u64, qty_1e8: i64, px_1e9: u64) -> bool {
        self.order_typed(order_id, 1, qty_1e8, px_1e9, OrderType::FOK)
    }

    /// Place FOK sell - must fill entire quantity or rejected.
    #[inline(always)]
    pub fn fok_sell(&mut self, order_id: u64, qty_1e8: i64, px_1e9: u64) -> bool {
        self.order_typed(order_id, -1, qty_1e8, px_1e9, OrderType::FOK)
    }

    // =========================================================================
    // POST-ONLY ORDERS - maker only, rejected if would cross book
    // =========================================================================

    /// Place a post-only buy order (maker only, rejected if would cross).
    #[inline(always)]
    pub fn post_only_buy(&mut self, order_id: u64, qty_1e8: i64, px_1e9: u64) -> bool {
        self.order_typed(order_id, 1, qty_1e8, px_1e9, OrderType::POST_ONLY)
    }

    /// Place a post-only sell order (maker only, rejected if would cross).
    #[inline(always)]
    pub fn post_only_sell(&mut self, order_id: u64, qty_1e8: i64, px_1e9: u64) -> bool {
        self.order_typed(order_id, -1, qty_1e8, px_1e9, OrderType::POST_ONLY)
    }

    // =========================================================================
    // CORE ORDER PLACEMENT
    // =========================================================================

    /// Place order with explicit type.
    #[inline(always)]
    pub fn order_typed(
        &mut self,
        order_id: u64,
        side: i8,
        qty_1e8: i64,
        px_1e9: u64,
        order_type: u8,
    ) -> bool {
        if self.len >= MAX_ACTIONS {
            return false;
        }
        self.actions[self.len] = Action {
            order_id,
            px_1e9,
            qty_1e8,
            side,
            is_cancel: 0,
            order_type,
            _pad: [0; 5],
        };
        self.len += 1;
        true
    }

    /// Cancel an order.
    #[inline(always)]
    pub fn cancel(&mut self, order_id: u64) -> bool {
        if self.len >= MAX_ACTIONS {
            return false;
        }
        self.actions[self.len] = Action {
            order_id,
            px_1e9: 0,
            qty_1e8: 0,
            side: 0,
            is_cancel: 1,
            order_type: 0,
            _pad: [0; 5],
        };
        self.len += 1;
        true
    }

    /// Cancel all open orders.
    #[inline(always)]
    pub fn cancel_all(&mut self, state: &AlgoState) {
        for i in 0..state.order_ct as usize {
            let o = &state.orders[i];
            if o.is_live() && self.len < MAX_ACTIONS {
                self.cancel(o.order_id);
            }
        }
    }

    /// Zero out an action at index (marks as no-op).
    /// Used by risk engine to neutralize rejected actions in-place.
    #[inline(always)]
    pub fn clear_at(&mut self, idx: usize) {
        if idx < self.len {
            self.actions[idx] = Action {
                order_id: 0,
                px_1e9: 0,
                qty_1e8: 0,
                side: 0,
                is_cancel: 0,
                order_type: 0,
                _pad: [0; 5],
            };
        }
    }

    #[inline(always)]
    pub fn get(&self, idx: usize) -> Option<&Action> {
        if idx < self.len {
            Some(&self.actions[idx])
        } else {
            None
        }
    }

    #[inline(always)]
    pub fn iter(&self) -> impl Iterator<Item = &Action> {
        self.actions[..self.len].iter()
    }
}

impl Default for Actions {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// ALGO TRAIT
// =============================================================================

/// Trading algorithm trait.
/// All methods run on HOT PATH - avoid heap allocations.
pub trait Algo: Send {
    /// Called on every book update.
    /// - book: L2 order book (20 levels)
    /// - state: Your position + open orders (server-managed)
    /// - actions: Buffer to place/cancel orders
    fn on_book(&mut self, book: &L2Book, state: &AlgoState, actions: &mut Actions);

    /// Order filled.
    fn on_fill(&mut self, fill: &Fill, state: &AlgoState);

    /// Order rejected.
    fn on_reject(&mut self, reject: &Reject);

    /// Shutdown - cancel open orders here.
    fn on_shutdown(&mut self, state: &AlgoState, actions: &mut Actions);
}

// =============================================================================
// WASM EXPORTS
// =============================================================================

/// Wire format for actions buffer.
#[repr(C)]
pub struct WasmActions {
    pub count: u32,
    pub _pad: u32,
    pub actions: [Action; MAX_ACTIONS],
}

impl WasmActions {
    pub const fn new() -> Self {
        Self {
            count: 0,
            _pad: 0,
            actions: [Action {
                order_id: 0,
                px_1e9: 0,
                qty_1e8: 0,
                side: 0,
                is_cancel: 0,
                order_type: 0,
                _pad: [0; 5],
            }; MAX_ACTIONS],
        }
    }

    pub fn from_actions(&mut self, actions: &Actions) {
        self.count = actions.len() as u32;
        for i in 0..actions.len() {
            self.actions[i] = actions.actions[i];
        }
    }
}

/// Generate WASM exports for your algo (only for WASM builds).
#[cfg(not(feature = "std"))]
#[macro_export]
macro_rules! export_algo {
    ($init:expr) => {
        static mut ALGO: Option<alloc::boxed::Box<dyn $crate::Algo>> = None;
        static mut ACTIONS: $crate::Actions = $crate::Actions::new();
        static mut WASM_OUT: $crate::WasmActions = $crate::WasmActions::new();

        #[inline(always)]
        fn init() {
            unsafe {
                if ALGO.is_none() {
                    ALGO = Some(alloc::boxed::Box::new($init));
                }
            }
        }

        #[no_mangle]
        pub extern "C" fn algo_on_book(book_ptr: u32, state_ptr: u32) -> u32 {
            init();
            unsafe {
                let book = &*(book_ptr as *const $crate::L2Book);
                let state = &*(state_ptr as *const $crate::AlgoState);
                ACTIONS.clear();
                if let Some(algo) = ALGO.as_mut() {
                    algo.on_book(book, state, &mut ACTIONS);
                }
                WASM_OUT.from_actions(&ACTIONS);
                &WASM_OUT as *const _ as u32
            }
        }

        #[no_mangle]
        pub extern "C" fn algo_on_fill(fill_ptr: u32, state_ptr: u32) {
            init();
            unsafe {
                let fill = &*(fill_ptr as *const $crate::Fill);
                let state = &*(state_ptr as *const $crate::AlgoState);
                if let Some(algo) = ALGO.as_mut() {
                    algo.on_fill(fill, state);
                }
            }
        }

        #[no_mangle]
        pub extern "C" fn algo_on_reject(reject_ptr: u32) {
            init();
            unsafe {
                let reject = &*(reject_ptr as *const $crate::Reject);
                if let Some(algo) = ALGO.as_mut() {
                    algo.on_reject(reject);
                }
            }
        }

        #[no_mangle]
        pub extern "C" fn algo_on_shutdown(state_ptr: u32) -> u32 {
            init();
            unsafe {
                let state = &*(state_ptr as *const $crate::AlgoState);
                ACTIONS.clear();
                if let Some(algo) = ALGO.as_mut() {
                    algo.on_shutdown(state, &mut ACTIONS);
                }
                WASM_OUT.from_actions(&ACTIONS);
                &WASM_OUT as *const _ as u32
            }
        }

        #[no_mangle]
        pub extern "C" fn algo_alloc(size: u32) -> u32 {
            let layout = core::alloc::Layout::from_size_align(size as usize, 8).unwrap();
            unsafe { alloc::alloc::alloc(layout) as u32 }
        }
    };
}

/// Stub macro for native builds (no-op).
#[cfg(feature = "std")]
#[macro_export]
macro_rules! export_algo {
    ($init:expr) => {};
}

// =============================================================================
// LOGGING - HFT-safe async logging
// =============================================================================

/// Log levels for algo logging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LogLevel {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

// Host import - only for WASM builds
#[cfg(not(feature = "std"))]
extern "C" {
    fn host_log_impl(level: u8, ptr: *const u8, len: u32);
}

#[cfg(not(feature = "std"))]
#[inline(always)]
fn host_log(level: u8, ptr: *const u8, len: u32) {
    unsafe {
        host_log_impl(level, ptr, len);
    }
}

// Stub for native builds (no-op)
#[cfg(feature = "std")]
#[inline(always)]
fn host_log(_level: u8, _ptr: *const u8, _len: u32) {}

/// Algo logging - non-blocking, ~100ns per call.
/// Logs are batched and viewable via dashboard with 1-2s delay.
pub mod log {
    use super::LogLevel;
    use core::fmt::Write;

    /// Log buffer for formatting.
    struct LogBuf {
        buf: [u8; 128],
        pos: usize,
    }

    impl LogBuf {
        #[inline(always)]
        const fn new() -> Self {
            Self {
                buf: [0u8; 128],
                pos: 0,
            }
        }

        #[inline(always)]
        fn as_slice(&self) -> &[u8] {
            &self.buf[..self.pos]
        }
    }

    impl Write for LogBuf {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let bytes = s.as_bytes();
            let space = self.buf.len() - self.pos;
            let n = bytes.len().min(space);
            self.buf[self.pos..self.pos + n].copy_from_slice(&bytes[..n]);
            self.pos += n;
            Ok(())
        }
    }

    #[inline(always)]
    fn send(level: LogLevel, msg: &[u8]) {
        super::host_log(level as u8, msg.as_ptr(), msg.len() as u32);
    }

    /// Log a trace message.
    #[inline(always)]
    pub fn trace(msg: &str) {
        send(LogLevel::Trace, msg.as_bytes());
    }

    /// Log a debug message.
    #[inline(always)]
    pub fn debug(msg: &str) {
        send(LogLevel::Debug, msg.as_bytes());
    }

    /// Log an info message.
    #[inline(always)]
    pub fn info(msg: &str) {
        send(LogLevel::Info, msg.as_bytes());
    }

    /// Log a warning message.
    #[inline(always)]
    pub fn warn(msg: &str) {
        send(LogLevel::Warn, msg.as_bytes());
    }

    /// Log an error message.
    #[inline(always)]
    pub fn error(msg: &str) {
        send(LogLevel::Error, msg.as_bytes());
    }

    /// Log with formatting (slightly slower).
    #[inline]
    pub fn log_fmt(level: LogLevel, args: core::fmt::Arguments<'_>) {
        let mut buf = LogBuf::new();
        let _ = buf.write_fmt(args);
        send(level, buf.as_slice());
    }

    /// Formatted info log.
    #[inline]
    pub fn info_fmt(args: core::fmt::Arguments<'_>) {
        log_fmt(LogLevel::Info, args);
    }

    /// Formatted warn log.
    #[inline]
    pub fn warn_fmt(args: core::fmt::Arguments<'_>) {
        log_fmt(LogLevel::Warn, args);
    }

    /// Formatted error log.
    #[inline]
    pub fn error_fmt(args: core::fmt::Arguments<'_>) {
        log_fmt(LogLevel::Error, args);
    }

    /// Formatted debug log.
    #[inline]
    pub fn debug_fmt(args: core::fmt::Arguments<'_>) {
        log_fmt(LogLevel::Debug, args);
    }
}

/// Log info message with formatting.
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::log::info_fmt(format_args!($($arg)*))
    };
}

/// Log warning with formatting.
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        $crate::log::warn_fmt(format_args!($($arg)*))
    };
}

/// Log error with formatting.
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::log::error_fmt(format_args!($($arg)*))
    };
}

/// Log debug with formatting.
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        $crate::log::debug_fmt(format_args!($($arg)*))
    };
}
