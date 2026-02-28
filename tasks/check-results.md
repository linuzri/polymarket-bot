# Post-Fix Checklist Results — Feb 28, 2026

## CHECK 1: Station Codes ✅ VERIFIED + 1 FIX

| City | Bot Station | Polymarket Resolution | Coordinates | Status |
|------|-------------|----------------------|-------------|--------|
| Chicago | KORD | "Chicago O'Hare Intl Airport Station" | Updated to O'Hare (41.9742, -87.9073) | ✅ Fixed coords |
| NYC | KLGA | "LaGuardia Airport Station" | 40.7128, -74.0060 (city center) | ✅ Correct station |
| Dallas | ~~KDFW~~ → **KDAL** | "Dallas Love Field Station" | Updated to Love Field (32.8471, -96.8518) | 🔴 **FIXED** |
| Miami | KMIA | "Miami Intl Airport Station" | 25.7617, -80.1918 (city center) | ✅ Correct station |

**Key finding:** Dallas was using DFW (KDFW) but Polymarket resolves on Love Field (KDAL). These airports are 20+ miles apart — systematic forecast error on every Dallas trade. Fixed station code AND coordinates.

**Chicago:** Station KORD was already correct per Polymarket. But coordinates were city center (41.8781) instead of O'Hare airport (41.9742). Updated to airport location for better forecast alignment.

**Note:** NYC and Miami still use city center coordinates, not airport coordinates. Low impact since airports are closer to city center for these cities, but could be refined later.

## CHECK 2: scan_log.jsonl ✅ EXISTS & WORKING

- File exists with structured JSONL entries
- Latest entry (2026-02-27T23:15): 39 markets discovered, 284 buckets evaluated, 0 trades placed
- All counter fields present: markets_discovered, markets_evaluated, markets_skipped_disagreement, buckets_evaluated, buckets_skipped_low_price, buckets_skipped_no_edge, etc.
- Zero trades placed since timezone fix — expected behavior, real edges are rarer

## CHECK 3: Disagreement Breaker ✅ WORKING

- **10 total mentions** of "disagree" in recent PM2 logs
- **1 market skip:** NYC Feb 27 — OM=37.9°F vs NOAA=46.0°F, gap=8.1°F (>8°F threshold)
- Subsequent scans showed NYC gap narrowed to 2.3°F — breaker correctly did NOT fire
- Breaker is functioning as designed

## CHECK 4: Timezone Date Alignment ⚠️ KNOWN LIMITATION

- `markets.rs` line 93: `let today = Utc::now().date_naive()` — uses UTC dates for market discovery
- Forecast API uses local timezone (e.g., `timezone=Asia/Seoul`)
- **Mismatch scenario:** At 23:15 UTC on Feb 27, Seoul (UTC+9) is already Feb 28. Bot discovers "Seoul Feb 27" market (UTC date) but forecast starts from Feb 28 (local date). No forecast data for Feb 27 → market silently skipped.
- **Impact:** Low — same-day markets for far-east cities (Seoul, Wellington, Tokyo) are nearly resolved anyway. Future-date markets (Feb 28, Mar 1) align correctly.
- **Not urgent.** Would only matter if we wanted to trade same-day markets for UTC+9 to UTC+13 cities. Not worth fixing during observation period.

## CHECK 5: Logging Infrastructure ✅ ALL 4 IMPLEMENTED

| Feature | Status | Details |
|---------|--------|---------|
| scan_log.jsonl | ✅ Implemented | Structured JSONL with all counters, written per scan |
| Outcome tracking (outcomes.rs) | ✅ Implemented | `check_outcomes()` called at scan start, writes to trade_outcomes.jsonl |
| Fill tracking | ✅ Implemented | `check_fill_status()` runs per scan, order_id + token_id saved in strategy_trades.json |
| Weekly Telegram summary | ✅ Implemented | `weekly_summary()` fires on Sunday (day change detection) |

**Note:** Most recent trades show `fill_confirmed: false` and `outcome: "NO_FILL"` — consistent with ladder orders at cheap prices not getting filled.
