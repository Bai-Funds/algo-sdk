//! XRP 5 USD Micro Market Maker (Kraken test profile)
//!
//! Strategy goals:
//! - Keep per-side quote notionals around $5
//! - Use maker-only orders (`post_only_*`) for fee-friendly testing
//! - Keep risk tight for small balances
//!
//! Build:
//! cargo build --target wasm32-unknown-unknown --release

#![cfg_attr(target_arch = "wasm32", no_std)]

#[cfg(target_arch = "wasm32")]
mod wasm {
    extern crate alloc;
    use algo_sdk::*;

    // -------------------------------------------------------------------------
    // CONFIG (XRP/USD on Kraken, small-balance safe defaults)
    // -------------------------------------------------------------------------

    // Keep a buffer for fees/holds so "$5 test accounts" don't constantly reject.
    const TARGET_NOTIONAL_USD_1E9: u64 = 4_250_000_000; // $4.25 per side effective
    const MIN_QTY_1E8: i64 = 100_000_000; // 1.0 XRP
    const MAX_QTY_1E8: i64 = 450_000_000; // 4.5 XRP

    const MAX_POSITION_1E8: i64 = 900_000_000; // 9.0 XRP max net inventory

    const MIN_SPREAD_BPS: u32 = 1;
    const MAX_SPREAD_BPS: u32 = 30;
    // Book-update based pacing. Keep this conservative for retail-size keys.
    const REQUOTE_EVERY_UPDATES: u64 = 10_000;
    const MIN_ACTION_GAP_UPDATES: u64 = 800;
    const REJECT_BACKOFF_BASE_UPDATES: u64 = 5_000;
    const REJECT_BACKOFF_MAX_UPDATES: u64 = 100_000;
    const LOG_EVERY_N_UPDATES: u64 = 200;
    const MAX_STALE_ENTRY_UPDATES: u64 = 2_200;
    const MAX_STALE_EXIT_UPDATES: u64 = 1_600;
    // Keep maker-first behavior; only force taker unwind in prolonged + losing cycles.
    const FORCE_EXIT_AGE_NS: u64 = 45_000_000_000; // 45s
    const FORCE_EXIT_MAX_UPNL_1E9: i64 = -5_000_000; // -$0.005 total uPnL

    struct Xrp5UsdMm {
        next_order_id: u64,
        bid_order_id: u64,
        ask_order_id: u64,
        bid_sent_ns: u64,
        ask_sent_ns: u64,
        bid_open_update: u64,
        ask_open_update: u64,
        cycle_start_ns: u64,

        update_count: u64,
        quote_count: u64,
        cancel_count: u64,
        fill_count: u64,
        reject_count: u64,
        cycle_count: u64,
        rt_total_ms: u64,
        rt_min_ms: u64,
        rt_max_ms: u64,

        last_action_update: u64,
        reject_streak: u32,
        backoff_until_update: u64,
    }

    impl Xrp5UsdMm {
        #[inline(always)]
        fn new() -> Self {
            Self {
                next_order_id: 8_000,
                bid_order_id: 0,
                ask_order_id: 0,
                bid_sent_ns: 0,
                ask_sent_ns: 0,
                bid_open_update: 0,
                ask_open_update: 0,
                cycle_start_ns: 0,
                update_count: 0,
                quote_count: 0,
                cancel_count: 0,
                fill_count: 0,
                reject_count: 0,
                cycle_count: 0,
                rt_total_ms: 0,
                rt_min_ms: u64::MAX,
                rt_max_ms: 0,
                last_action_update: 0,
                reject_streak: 0,
                backoff_until_update: 0,
            }
        }

        #[inline(always)]
        fn gen_order_id(&mut self) -> u64 {
            self.next_order_id = self.next_order_id.saturating_add(1);
            self.next_order_id
        }

        #[inline(always)]
        fn target_qty_1e8(mid_px_1e9: u64) -> i64 {
            if mid_px_1e9 == 0 {
                return MIN_QTY_1E8;
            }
            let raw = ((TARGET_NOTIONAL_USD_1E9 as u128) * 100_000_000u128 / (mid_px_1e9 as u128)) as i64;
            raw.clamp(MIN_QTY_1E8, MAX_QTY_1E8)
        }

        #[inline(always)]
        fn action_gap_ok(&self) -> bool {
            self.update_count.saturating_sub(self.last_action_update) >= MIN_ACTION_GAP_UPDATES
        }

        #[inline(always)]
        fn requote_due(&self) -> bool {
            self.update_count.saturating_sub(self.last_action_update) >= REQUOTE_EVERY_UPDATES
        }

        #[inline(always)]
        fn cancel_open_quotes(&mut self, actions: &mut Actions) {
            if self.bid_order_id != 0 {
                actions.cancel(self.bid_order_id);
                self.bid_order_id = 0;
                self.bid_sent_ns = 0;
                self.bid_open_update = 0;
                self.cancel_count = self.cancel_count.saturating_add(1);
            }
            if self.ask_order_id != 0 {
                actions.cancel(self.ask_order_id);
                self.ask_order_id = 0;
                self.ask_sent_ns = 0;
                self.ask_open_update = 0;
                self.cancel_count = self.cancel_count.saturating_add(1);
            }
            self.last_action_update = self.update_count;
        }

        #[inline(always)]
        fn qty_fmt_parts(qty_1e8: i64) -> (&'static str, u64, u64) {
            let sign = if qty_1e8 < 0 { "-" } else { "" };
            let abs = qty_1e8.unsigned_abs();
            (sign, abs / 100_000_000, (abs / 10_000) % 10_000)
        }

        #[inline(always)]
        fn usd_fmt_parts(value_1e9: i64) -> (&'static str, u64, u64) {
            let sign = if value_1e9 < 0 { "-" } else { "" };
            let abs = value_1e9.unsigned_abs();
            (sign, abs / 1_000_000_000, (abs / 1_000_000) % 1_000)
        }
    }

    impl Algo for Xrp5UsdMm {
        fn on_book(&mut self, book: &L2Book, state: &AlgoState, actions: &mut Actions) {
            self.update_count = self.update_count.saturating_add(1);

            let best_bid = match book.best_bid() {
                Some(lvl) => lvl.px_1e9,
                None => return,
            };
            let best_ask = match book.best_ask() {
                Some(lvl) => lvl.px_1e9,
                None => return,
            };
            if best_bid == 0 || best_ask == 0 || best_ask <= best_bid {
                return;
            }

            let mid = book.mid_px_1e9();
            let spread_bps = book.spread_bps();

            if self.update_count == 1 {
                let qty_1e8 = Self::target_qty_1e8(mid);
                log_warn!("=== XRP 5USD MICRO MM (LIVE) ===");
                log_info!(
                    "INIT: bid=${}.{:03} ask=${}.{:03} spread={}bps target_qty={} XRP",
                    best_bid / 1_000_000_000,
                    (best_bid / 1_000_000) % 1000,
                    best_ask / 1_000_000_000,
                    (best_ask / 1_000_000) % 1000,
                    spread_bps,
                    qty_1e8 / 100_000_000
                );
                log_info!(
                    "LIMITS: notional=${}.{:02}/side max_pos={} XRP spread=[{},{}]bps",
                    TARGET_NOTIONAL_USD_1E9 / 1_000_000_000,
                    (TARGET_NOTIONAL_USD_1E9 / 10_000_000) % 100,
                    MAX_POSITION_1E8 / 100_000_000,
                    MIN_SPREAD_BPS,
                    MAX_SPREAD_BPS
                );
                log_info!("MODE: maker-first; taker-unwind only on stale+losing cycles");
            }

            if self.update_count % LOG_EVERY_N_UPDATES == 0 {
                let (r_sign, r_whole, r_frac3) = Self::usd_fmt_parts(state.realized_pnl_1e9);
                let (u_sign, u_whole, u_frac3) = Self::usd_fmt_parts(state.unrealized_pnl_1e9);
                let (t_sign, t_whole, t_frac3) = Self::usd_fmt_parts(state.total_pnl_1e9());
                let (p_sign, p_whole, p_frac4) = Self::qty_fmt_parts(state.position_1e8);
                let avg_rt_ms = if self.cycle_count > 0 {
                    self.rt_total_ms / self.cycle_count
                } else {
                    0
                };
                let min_rt_ms = if self.rt_min_ms == u64::MAX { 0 } else { self.rt_min_ms };
                log_info!(
                    "S[{}] p={}{}.{:04} o={} q={} f={} r={} c={} sp={} rt={}/{}/{} pn={}{}.{:03}/{}{}.{:03}/{}{}.{:03}",
                    self.update_count,
                    p_sign, p_whole, p_frac4,
                    state.order_ct,
                    self.quote_count,
                    self.fill_count,
                    self.reject_count,
                    self.cancel_count,
                    spread_bps,
                    avg_rt_ms, min_rt_ms, self.rt_max_ms,
                    r_sign, r_whole, r_frac3,
                    u_sign, u_whole, u_frac3,
                    t_sign, t_whole, t_frac3
                );
            }

            // Back off aggressively after exchange/local rejects to avoid rate-limit spirals.
            if self.update_count < self.backoff_until_update {
                return;
            }

            // Market out-of-regime: pull quotes and wait.
            if spread_bps < MIN_SPREAD_BPS || spread_bps > MAX_SPREAD_BPS {
                if (self.bid_order_id != 0 || self.ask_order_id != 0) && self.action_gap_ok() {
                    self.cancel_open_quotes(actions);
                }
                return;
            }

            // Hard inventory stop.
            if state.position_1e8.abs() >= MAX_POSITION_1E8 {
                if (self.bid_order_id != 0 || self.ask_order_id != 0) && self.action_gap_ok() {
                    log_warn!("RISK: max position reached, pulling quotes");
                    self.cancel_open_quotes(actions);
                }
                return;
            }

            // Requote periodically so prices do not go stale.
            if (self.bid_order_id != 0 || self.ask_order_id != 0) && self.requote_due() && self.action_gap_ok() {
                self.cancel_open_quotes(actions);
                return;
            }

            // Avoid spamming actions.
            if !self.action_gap_ok() {
                return;
            }

            // If edge state already has many tracked orders, avoid adding more.
            if state.order_ct >= 2 {
                return;
            }

            let base_qty_1e8 = Self::target_qty_1e8(mid);
            let mut bid_qty_1e8 = 0i64;
            let mut ask_qty_1e8 = 0i64;

            // Cash-safe mode for small accounts:
            // - If flat/short, only post BUYs.
            // - If long, only post SELLs to flatten.
            // This avoids requiring both USD and XRP inventory simultaneously.
            if state.position_1e8 <= 0 {
                if self.ask_order_id != 0 {
                    actions.cancel(self.ask_order_id);
                    self.ask_order_id = 0;
                    self.ask_sent_ns = 0;
                    self.ask_open_update = 0;
                    self.cancel_count = self.cancel_count.saturating_add(1);
                    self.last_action_update = self.update_count;
                    return;
                }
                bid_qty_1e8 = base_qty_1e8;
            } else {
                if self.bid_order_id != 0 {
                    actions.cancel(self.bid_order_id);
                    self.bid_order_id = 0;
                    self.bid_sent_ns = 0;
                    self.bid_open_update = 0;
                    self.cancel_count = self.cancel_count.saturating_add(1);
                    self.last_action_update = self.update_count;
                    return;
                }
                ask_qty_1e8 = state.position_1e8.min(MAX_QTY_1E8);
            }

            // Respect outstanding risk envelope (position + live working qty).
            let projected_long_1e8 = state.position_1e8.saturating_add(state.open_buy_qty_1e8());
            let projected_short_1e8 = state.position_1e8.saturating_sub(state.open_sell_qty_1e8());

            if projected_long_1e8.saturating_add(bid_qty_1e8) > MAX_POSITION_1E8 {
                bid_qty_1e8 = 0;
            }
            if projected_short_1e8.saturating_sub(ask_qty_1e8) < -MAX_POSITION_1E8 {
                ask_qty_1e8 = 0;
            }

            // Use top-of-book prices to stay on valid tick increments.
            if self.bid_order_id == 0 && bid_qty_1e8 > 0 {
                let order_id = self.gen_order_id();
                if actions.post_only_buy(order_id, bid_qty_1e8, best_bid) {
                    self.bid_order_id = order_id;
                    self.bid_sent_ns = book.recv_ns;
                    self.bid_open_update = self.update_count;
                    self.quote_count = self.quote_count.saturating_add(1);
                    self.last_action_update = self.update_count;
                    let (_q_sign, q_whole, q_frac4) = Self::qty_fmt_parts(bid_qty_1e8);
                    log_info!(
                        "QUOTE: BUY order={} qty={}.{:04} @${}.{:03}",
                        order_id,
                        q_whole,
                        q_frac4,
                        best_bid / 1_000_000_000,
                        (best_bid / 1_000_000) % 1000
                    );
                }
            }

            // If entry quote has rested too long without fill, pull and retry later.
            if self.bid_order_id != 0
                && self.update_count.saturating_sub(self.bid_open_update) > MAX_STALE_ENTRY_UPDATES
                && self.action_gap_ok()
            {
                actions.cancel(self.bid_order_id);
                self.bid_order_id = 0;
                self.bid_sent_ns = 0;
                self.bid_open_update = 0;
                self.cancel_count = self.cancel_count.saturating_add(1);
                self.last_action_update = self.update_count;
                return;
            }

            // Demo mode: if we're long and an exit quote is stale for too long, force flatten.
            if state.position_1e8 > 0
                && self.ask_order_id != 0
                && self.update_count.saturating_sub(self.ask_open_update) > MAX_STALE_EXIT_UPDATES
                && self.action_gap_ok()
            {
                actions.cancel(self.ask_order_id);
                self.ask_order_id = 0;
                self.ask_sent_ns = 0;
                self.ask_open_update = 0;
                self.cancel_count = self.cancel_count.saturating_add(1);
                self.last_action_update = self.update_count;
                return;
            }

            if self.ask_order_id == 0 && ask_qty_1e8 > 0 {
                if self.cycle_start_ns > 0
                    && book.recv_ns.saturating_sub(self.cycle_start_ns) > FORCE_EXIT_AGE_NS
                    && state.unrealized_pnl_1e9 <= FORCE_EXIT_MAX_UPNL_1E9
                {
                    let order_id = self.gen_order_id();
                    if actions.ioc_sell(order_id, ask_qty_1e8, best_bid) {
                        self.ask_order_id = order_id;
                        self.ask_sent_ns = book.recv_ns;
                        self.ask_open_update = self.update_count;
                        self.quote_count = self.quote_count.saturating_add(1);
                        self.last_action_update = self.update_count;
                        let (_q_sign, q_whole, q_frac4) = Self::qty_fmt_parts(ask_qty_1e8);
                        log_warn!(
                            "UNWIND: IOC SELL order={} qty={}.{:04} @${}.{:03}",
                            order_id,
                            q_whole,
                            q_frac4,
                            best_bid / 1_000_000_000,
                            (best_bid / 1_000_000) % 1000
                        );
                    }
                    return;
                }
                let order_id = self.gen_order_id();
                if actions.post_only_sell(order_id, ask_qty_1e8, best_ask) {
                    self.ask_order_id = order_id;
                    self.ask_sent_ns = book.recv_ns;
                    self.ask_open_update = self.update_count;
                    self.quote_count = self.quote_count.saturating_add(1);
                    self.last_action_update = self.update_count;
                    let (_q_sign, q_whole, q_frac4) = Self::qty_fmt_parts(ask_qty_1e8);
                    log_info!(
                        "QUOTE: SELL order={} qty={}.{:04} @${}.{:03}",
                        order_id,
                        q_whole,
                        q_frac4,
                        best_ask / 1_000_000_000,
                        (best_ask / 1_000_000) % 1000
                    );
                }
            }
        }

        fn on_fill(&mut self, fill: &Fill, state: &AlgoState) {
            self.fill_count = self.fill_count.saturating_add(1);
            self.reject_streak = 0;
            let mut order_latency_ms = 0u64;
            if fill.order_id == self.bid_order_id {
                if self.bid_sent_ns > 0 {
                    order_latency_ms = fill.recv_ns.saturating_sub(self.bid_sent_ns) / 1_000_000;
                }
                if state.find_order(fill.order_id).is_none() {
                    self.bid_order_id = 0;
                    self.bid_sent_ns = 0;
                    self.bid_open_update = 0;
                }
                if state.position_1e8 > 0 && self.cycle_start_ns == 0 {
                    self.cycle_start_ns = fill.recv_ns;
                }
            }
            if fill.order_id == self.ask_order_id {
                if self.ask_sent_ns > 0 {
                    order_latency_ms = fill.recv_ns.saturating_sub(self.ask_sent_ns) / 1_000_000;
                }
                if state.find_order(fill.order_id).is_none() {
                    self.ask_order_id = 0;
                    self.ask_sent_ns = 0;
                    self.ask_open_update = 0;
                }
            }

            let side = if fill.side > 0 { "BUY" } else { "SELL" };
            let (_q_sign, q_whole, q_frac4) = Self::qty_fmt_parts(fill.qty_1e8);
            let (p_sign, p_whole, p_frac3) = Self::qty_fmt_parts(state.position_1e8);
            let (r_sign, r_whole, r_frac3) = Self::usd_fmt_parts(state.realized_pnl_1e9);
            let (u_sign, u_whole, u_frac3) = Self::usd_fmt_parts(state.unrealized_pnl_1e9);
            let (t_sign, t_whole, t_frac3) = Self::usd_fmt_parts(state.total_pnl_1e9());

            let mut rt_log = (0u64, false);
            if fill.side < 0 && self.cycle_start_ns > 0 && state.position_1e8 == 0 {
                let rt_ms = fill.recv_ns.saturating_sub(self.cycle_start_ns) / 1_000_000;
                self.cycle_count = self.cycle_count.saturating_add(1);
                self.rt_total_ms = self.rt_total_ms.saturating_add(rt_ms);
                if rt_ms < self.rt_min_ms {
                    self.rt_min_ms = rt_ms;
                }
                if rt_ms > self.rt_max_ms {
                    self.rt_max_ms = rt_ms;
                }
                self.cycle_start_ns = 0;
                rt_log = (rt_ms, true);
            }

            log_warn!(
                "FILL: {} order={} qty={}.{:04} @${}.{:03} latency={}ms pos={}{}.{:04} pnl=r${}{}.{:03} u${}{}.{:03} t${}{}.{:03}",
                side,
                fill.order_id,
                q_whole, q_frac4,
                fill.px_1e9 / 1_000_000_000,
                (fill.px_1e9 / 1_000_000) % 1000,
                order_latency_ms,
                p_sign, p_whole, p_frac3,
                r_sign, r_whole, r_frac3,
                u_sign, u_whole, u_frac3,
                t_sign, t_whole, t_frac3
            );

            if rt_log.1 {
                log_warn!(
                    "ROUND_TRIP: cycle={} rt={}ms min={}ms max={}ms",
                    self.cycle_count,
                    rt_log.0,
                    if self.rt_min_ms == u64::MAX { 0 } else { self.rt_min_ms },
                    self.rt_max_ms
                );
            }
        }

        fn on_reject(&mut self, reject: &Reject) {
            self.reject_count = self.reject_count.saturating_add(1);
            if reject.order_id == self.bid_order_id {
                self.bid_order_id = 0;
                self.bid_sent_ns = 0;
                self.bid_open_update = 0;
            }
            if reject.order_id == self.ask_order_id {
                self.ask_order_id = 0;
                self.ask_sent_ns = 0;
                self.ask_open_update = 0;
            }

            self.reject_streak = (self.reject_streak + 1).min(8);
            let exp = self.reject_streak.saturating_sub(1).min(3);
            let mult = 1u64 << exp;
            let delay = (REJECT_BACKOFF_BASE_UPDATES.saturating_mul(mult))
                .min(REJECT_BACKOFF_MAX_UPDATES);
            self.backoff_until_update = self.update_count.saturating_add(delay);

            log_error!(
                "REJECT: order={} code={} reason={}",
                reject.order_id,
                reject.code,
                reject.reason()
            );
            log_warn!(
                "BACKOFF: reject_streak={} wait_updates={}",
                self.reject_streak,
                delay
            );
        }

        fn on_shutdown(&mut self, state: &AlgoState, actions: &mut Actions) {
            log_warn!(
                "SHUTDOWN: updates={} fills={} rejects={} quotes={} cancels={}",
                self.update_count,
                self.fill_count,
                self.reject_count,
                self.quote_count,
                self.cancel_count
            );
            actions.cancel_all(state);
        }
    }

    export_algo!(Xrp5UsdMm::new());

    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo) -> ! {
        loop {}
    }

    #[global_allocator]
    static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;
}

#[cfg(not(target_arch = "wasm32"))]
mod native {}
