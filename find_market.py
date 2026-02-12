import requests
r = requests.get('https://gamma-api.polymarket.com/markets?closed=false&limit=20&order=volume&ascending=false&active=true')
for m in r.json():
    prices = m.get('outcomePrices','')
    if prices:
        prices = prices.strip('[]').replace('"','').split(',')
        if len(prices) >= 2:
            yes_p = float(prices[0])
            if 0.1 < yes_p < 0.9:
                slug = m["slug"][:55]
                vol = m.get("volume","?")
                print(f"{slug:55} YES:{yes_p:.2f} vol:{vol}")
