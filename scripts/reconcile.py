"""
Polymarket Portfolio Reconciliation
Pulls all trades from CLOB API and reconciles against portfolio_state.json
"""
from py_clob_client.client import ClobClient
from py_clob_client.clob_types import ApiCreds, BalanceAllowanceParams
from dotenv import load_dotenv
import json
import os

load_dotenv()

# Setup client
key = os.getenv('POLY_PRIVATE_KEY')
c = ClobClient(
    'https://clob.polymarket.com',
    chain_id=137,
    key=key,
    funder=os.getenv('POLY_PROXY_WALLET'),
    signature_type=1
)
creds = c.derive_api_key()
c2 = ClobClient(
    'https://clob.polymarket.com',
    chain_id=137,
    key=key,
    creds=ApiCreds(api_key=creds.api_key, api_secret=creds.api_secret, api_passphrase=creds.api_passphrase),
    funder=os.getenv('POLY_PROXY_WALLET'),
    signature_type=1
)

print("=" * 70)
print("POLYMARKET PORTFOLIO RECONCILIATION")
print("=" * 70)

# 1. Get balance
try:
    params = BalanceAllowanceParams(asset_type='COLLATERAL')
    bal = c2.get_balance_allowance(params)
    balance_raw = int(bal['balance'])
    balance_usd = balance_raw / 1_000_000  # USDC has 6 decimals
    print(f"\n1. EXCHANGE BALANCE: ${balance_usd:.6f}")
except Exception as e:
    print(f"\n1. BALANCE ERROR: {e}")
    balance_usd = 0

# 2. Get all trades
print(f"\n2. FETCHING ALL TRADES FROM CLOB API...")
try:
    trades = c2.get_trades()
    print(f"   Found {len(trades)} trades")
except Exception as e:
    print(f"   ERROR: {e}")
    trades = []

# 3. Get open orders
print(f"\n3. FETCHING OPEN ORDERS...")
try:
    orders = c2.get_orders()
    if isinstance(orders, list):
        print(f"   Found {len(orders)} open orders")
    else:
        print(f"   Response: {str(orders)[:200]}")
        orders = []
except Exception as e:
    print(f"   ERROR: {e}")
    orders = []

# 4. Analyze trades
print(f"\n4. TRADE ANALYSIS")
print("-" * 70)

total_spent = 0  # Money going out (buying)
total_received = 0  # Money coming in (selling/resolution)
buy_count = 0
sell_count = 0

trades_by_market = {}

for t in trades:
    # Trade structure varies - let's inspect first
    if isinstance(t, dict):
        side = t.get('side', 'unknown')
        price = float(t.get('price', 0))
        size = float(t.get('size', 0))
        market = t.get('market', t.get('asset_id', 'unknown'))
        trade_type = t.get('type', t.get('trade_type', 'unknown'))
        maker_address = t.get('maker_address', '')
        taker_address = t.get('taker_address', '')
        timestamp = t.get('match_time', t.get('created_at', 'unknown'))
        
        cost = price * size
        
        # Determine if we were buyer or seller
        proxy = os.getenv('POLY_PROXY_WALLET', '').lower()
        is_maker = maker_address.lower() == proxy if proxy else False
        is_taker = taker_address.lower() == proxy if proxy else False
        
        role = 'MAKER' if is_maker else 'TAKER' if is_taker else '?'
        
        # If we're the maker and side is BUY, we bought
        # If we're the taker and side is BUY, the maker bought (we sold)
        if is_maker:
            if side == 'BUY':
                total_spent += cost
                buy_count += 1
            else:
                total_received += cost
                sell_count += 1
        elif is_taker:
            if side == 'BUY':
                total_received += cost  # Taker on buy side = maker sold to us... wait
                sell_count += 1
            else:
                total_spent += cost
                buy_count += 1
        
        market_key = market[:20] if len(market) > 20 else market
        if market_key not in trades_by_market:
            trades_by_market[market_key] = []
        trades_by_market[market_key].append({
            'side': side, 'price': price, 'size': size, 'cost': cost,
            'role': role, 'time': timestamp
        })
        
        print(f"   {timestamp[:19] if len(str(timestamp))>19 else timestamp} | {role:5} | {side:4} | {size:>10.2f} @ ${price:.4f} = ${cost:.4f}")
    else:
        print(f"   Raw trade: {str(t)[:200]}")

print(f"\n{'=' * 70}")
print(f"SUMMARY")
print(f"{'=' * 70}")
print(f"Total trades: {len(trades)}")
print(f"Buy trades: {buy_count}, Sell trades: {sell_count}")
print(f"Total spent (buys): ${total_spent:.4f}")
print(f"Total received (sells): ${total_received:.4f}")
print(f"Net flow: ${total_received - total_spent:.4f}")
print(f"Exchange balance: ${balance_usd:.6f}")
print(f"Initial deposit: $100.27")
print(f"Accounted for: ${balance_usd + total_spent - total_received:.4f}")
print(f"Missing: ${100.27 - balance_usd - (total_spent - total_received):.4f}")

# 5. Compare with portfolio_state.json
print(f"\n{'=' * 70}")
print(f"PORTFOLIO STATE COMPARISON")
print(f"{'=' * 70}")

ps_path = os.path.join(os.path.dirname(os.path.dirname(__file__)), 'portfolio_state.json')
if os.path.exists(ps_path):
    ps = json.load(open(ps_path))
    positions = ps.get('positions', {})
    resolved = ps.get('resolved', [])
    
    ps_invested = sum(p.get('cost_basis', 0) for p in positions.values())
    ps_resolved_pnl = sum(r.get('realized_pnl', 0) for r in resolved)
    
    print(f"Portfolio state:")
    print(f"  Open positions: {len(positions)}")
    print(f"  Total invested: ${ps_invested:.2f}")
    print(f"  Resolved: {len(resolved)}")
    print(f"  Resolved P/L: ${ps_resolved_pnl:.2f}")
    
    print(f"\nReconciliation:")
    print(f"  Deposit:          $100.27")
    print(f"  - In positions:   ${ps_invested:.2f}")
    print(f"  - Resolved loss:  ${abs(ps_resolved_pnl):.2f}")
    print(f"  = Expected bal:   ${100.27 - ps_invested + ps_resolved_pnl:.2f}")
    print(f"  Actual balance:   ${balance_usd:.2f}")
    print(f"  Difference:       ${balance_usd - (100.27 - ps_invested + ps_resolved_pnl):.2f}")

# 6. Open orders value
if orders:
    order_value = 0
    for o in orders:
        if isinstance(o, dict):
            p = float(o.get('price', 0))
            s = float(o.get('size', o.get('original_size', 0)))
            order_value += p * s
    print(f"\n  Open orders value: ${order_value:.2f}")
    print(f"  Adjusted expected: ${100.27 - ps_invested + ps_resolved_pnl - order_value:.2f}")

# Save raw data for inspection
output = {
    'balance_usd': balance_usd,
    'trades': trades if isinstance(trades, list) else [],
    'open_orders': orders if isinstance(orders, list) else [],
    'total_spent': total_spent,
    'total_received': total_received,
}
with open('reconciliation_data.json', 'w') as f:
    json.dump(output, f, indent=2, default=str)
print(f"\nRaw data saved to reconciliation_data.json")
