# Agent Checklist: Post-Fix Log Review (Feb 27)

> These are investigation tasks ONLY. Do NOT change any trading logic unless explicitly noted.
> The bot is running and should NOT be restarted unless a fix requires it.

---

## CHECK 1: Chicago Station Code — KORD vs KMDW

The logs show `Resolution station: KORD` for Chicago. But Polymarket Chicago weather markets resolve using Weather Underground data, and Chicago Midway Airport (KMDW) is commonly the resolution station — not O'Hare (KORD). These stations can differ by 2-3°F.

**Action:**
1. Check which Weather Underground station Polymarket actually uses to resolve Chicago markets. Look at a recently resolved Chicago market on Polymarket and see the reported temperature source.
2. Check what station code is set in the `City` struct for Chicago in `mod.rs`.
3. If Polymarket uses KMDW (Midway), change the station code from `"KORD"` to `"KMDW"`. The coordinates should also be updated to Midway: `(41.7868, -87.7522)` instead of O'Hare.
4. While you're at it, verify ALL other US city station codes match what Polymarket/Weather Underground actually uses for resolution. Cross-check at least NYC (KLGA vs KNYC vs KJFK), Dallas (KDFW vs KDAL), and Miami (KMIA).

**Report back:** Which station Polymarket uses for Chicago, and whether any station codes needed changing.

---

## CHECK 2: Zero Trades Is Expected — No Action Needed

284 buckets evaluated, zero edges found. This is expected behavior after the timezone fix removed phantom edges. The bot now needs to find REAL edges, which are rarer.

**Action:** No changes needed. Do NOT lower `min_edge` or any other threshold. We are in a 1-week observation period. Just confirm:
1. Is the scan summary being written to `scan_log.jsonl`? Check that the file exists and has at least one entry.
2. Does the JSONL entry include all the counter fields (markets_discovered, markets_evaluated, buckets_evaluated, buckets_skipped_no_edge, etc.)?

If `scan_log.jsonl` doesn't exist or isn't being written, implement TASK 1 from `logging-improvements.md`.

**Report back:** Whether `scan_log.jsonl` exists and contains structured data.

---

## CHECK 3: Model Disagreement Breaker — Confirm It Works

The disagreement breaker fired on NYC in an earlier scan (OM=37.9 vs NOAA=46.0, 8.1°F gap) but didn't fire in this scan (OM=38.7 vs NOAA=41.0, 2.3°F gap). This is correct — the models updated and now agree more closely.

**Action:** No changes needed. Just confirm:
1. Search the PM2 logs for any `MODEL DISAGREEMENT` or `NOAA DISAGREES` warnings from the past few hours:
```bash
pm2 logs polymarket-bot --lines 5000 | grep -i "disagree"
```
2. Report how many times the breaker has fired since the bot was restarted.

**Report back:** Count of disagreement breaker activations and which cities/dates triggered them.

---

## CHECK 4: Seoul Missing Feb 27 Forecast — Date Matching Issue?

The logs show Seoul ensemble data for Feb 28, Mar 1, Mar 2 — but there's a market for "Seoul on February 27". Seoul is UTC+9, so when the bot runs at 23:15 UTC on Feb 27, it's already 08:15 AM on Feb 28 in Seoul. Feb 27 is essentially over in Seoul local time.

**Action:** Investigate whether this is causing a problem:
1. Check if the bot tried to evaluate the Seoul Feb 27 market. Search logs:
```bash
pm2 logs polymarket-bot --lines 5000 | grep -i "seoul.*feb.*27\|seoul.*2026-02-27"
```
2. If the bot fetched a forecast for Seoul Feb 27 but it didn't show up in the ensemble logs, there may be a date alignment issue — the ensemble API with `timezone=Asia/Seoul` might return dates starting from Feb 28 (local "today") instead of Feb 27.
3. Check: does the market discovery find markets by UTC date or local date? If it finds "Seoul Feb 27" but the forecast API returns data starting from local-today (Feb 28 in Seoul), the dates won't match and that market gets no forecast → no evaluation.

**This is NOT urgent** — same-day markets in far-east timezones are low-value anyway since they're nearly resolved. But if the same pattern happens with Seoul Feb 28 or Mar 1 markets (future dates that SHOULD have forecasts), that's a real bug.

**Report back:** Whether Seoul Feb 27 was evaluated or silently skipped, and whether the forecast dates align correctly with market dates for Seoul/Tokyo/Wellington.

---

## CHECK 5: Logging Improvements Status

The `logging-improvements.md` file contains 4 tasks for enabling proper performance evaluation after 1 week. These should be implemented without changing any trading logic.

**Action:** Check the status of each:
1. **Scan summary log** (`scan_log.jsonl`) — Is it being written? The last line of the scan shows `SCAN SUMMARY` in the console log, but is it also being appended to the JSONL file?
2. **Outcome tracking** (`trade_outcomes.jsonl`) — Is `outcomes.rs` implemented? Is `check_outcomes()` being called at the start of each scan?
3. **Fill tracking** — Are order IDs being saved in `strategy_trades.json`? Is `check_fill_status()` running?
4. **Weekly Telegram summary** — Is the reporting logic implemented? (This one is lowest priority since we don't have a full week yet.)

If any of these are NOT implemented yet, implement them now following the instructions in `logging-improvements.md`. These are logging-only changes — they do not affect trading.

**Report back:** Status of each of the 4 logging tasks (implemented/not implemented).

---

## Summary

```
CHECK 1: Verify Chicago uses KMDW not KORD. Fix if wrong. Check other cities too.
CHECK 2: Confirm scan_log.jsonl exists and is being written correctly.
CHECK 3: Report count of disagreement breaker activations from recent logs.
CHECK 4: Investigate whether Seoul/Tokyo/Wellington forecast dates align with market dates across timezones.
CHECK 5: Report status of all 4 logging improvement tasks. Implement any that are missing.
```

Do NOT change min_edge, kelly_fraction, max_total_exposure, or any trading parameters.
We are in a 1-week observation period. Logging and station code accuracy only.

---

*Generated Feb 28, 2026 — post-fix log review checklist*
