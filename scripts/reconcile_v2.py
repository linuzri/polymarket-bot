"""
Deep reconciliation - match every trade to understand money flow
"""
import json

data = json.load(open('reconciliation_data.json'))
trades = data['trades']

print("=" * 70)
print("DEEP TRADE ANALYSIS")
print("=" * 70)

# Group by asset_id (token)
by_token = {}
for t in trades:
    token = t.get('asset_id', 'unknown')
    if token not in by_token:
        by_token[token] = {'buys': [], 'sells': [], 'market': t.get('market', '')}
    side = t.get('side', '')
    price = float(t.get('price', 0))
    size = float(t.get('size', 0))
    cost = price * size
    entry = {'price': price, 'size': size, 'cost': cost, 'time': t.get('match_time', '')}
    if side == 'BUY':
        by_token[token]['buys'].append(entry)
    else:
        by_token[token]['sells'].append(entry)

print(f"\nUnique tokens traded: {len(by_token)}")
print()

total_locked = 0  # Money locked in unfilled/open positions
total_lost = 0    # Money lost on sells below buy price
total_net = 0

for token, data_t in by_token.items():
    buys = data_t['buys']
    sells = data_t['sells']
    buy_total = sum(b['cost'] for b in buys)
    sell_total = sum(s['cost'] for s in sells)
    buy_shares = sum(b['size'] for b in buys)
    sell_shares = sum(s['size'] for s in sells)
    net_shares = buy_shares - sell_shares
    net_cost = buy_total - sell_total
    
    status = "CLOSED" if abs(net_shares) < 0.01 else "OPEN"
    if status == "OPEN":
        total_locked += net_cost
    
    total_net += net_cost
    
    # Truncate token for display
    token_short = token[:16] + "..." if len(token) > 16 else token
    
    if net_cost > 0.01 or net_cost < -0.01:  # Skip dust
        pnl_label = f"LOCKED ${net_cost:.2f}" if status == "OPEN" else f"P/L ${-net_cost:.2f}"
        print(f"  {status:6} | Bought: {buy_shares:>8.1f} (${buy_total:>8.2f}) | Sold: {sell_shares:>8.1f} (${sell_total:>8.2f}) | Net: {net_shares:>8.1f} shares | {pnl_label}")

print(f"\n{'=' * 70}")
print(f"MONEY FLOW SUMMARY")
print(f"{'=' * 70}")
print(f"Initial deposit:           $100.27")
print(f"Total bought:              ${sum(sum(b['cost'] for b in d['buys']) for d in by_token.values()):.2f}")
print(f"Total sold:                ${sum(sum(s['cost'] for s in d['sells']) for d in by_token.values()):.2f}")
print(f"Net spent (buys - sells):  ${total_net:.2f}")
print(f"Exchange balance:          $25.63")
print(f"Balance + net spent:       ${25.63 + total_net:.2f}")
print(f"vs Deposit:                $100.27")
print(f"Unaccounted:               ${100.27 - 25.63 - total_net:.2f}")
print()
print(f"Money locked in open positions (net buys): ${total_locked:.2f}")
print(f"Sniper orders that FILLED and are waiting resolution:")

# The sniper orders are the big ones at 0.96-0.999 prices
for token, data_t in by_token.items():
    buys = data_t['buys']
    sells = data_t['sells']
    buy_shares = sum(b['size'] for b in buys)
    sell_shares = sum(s['size'] for s in sells)
    net_shares = buy_shares - sell_shares
    if net_shares > 0.5:
        avg_price = sum(b['cost'] for b in buys) / buy_shares if buy_shares > 0 else 0
        net_cost = sum(b['cost'] for b in buys) - sum(s['cost'] for s in sells)
        if avg_price >= 0.90:
            print(f"  {net_shares:.1f} shares @ avg ${avg_price:.4f} = ${net_cost:.2f} locked")
