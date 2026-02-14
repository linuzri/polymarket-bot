"""
BTC 5-min prediction script for Polymarket bot.
Connects to MT5, gets M5 candles, runs ensemble models, outputs JSON.
"""

import sys
import os
import json
import numpy as np
import pandas as pd

# Add mt5-trading paths
MT5_PROJECT = r"C:\Users\Nazri Hussain\projects\mt5-trading"
BTCUSD_DIR = os.path.join(MT5_PROJECT, "btcusd")
ML_DIR = os.path.join(BTCUSD_DIR, "ml")

sys.path.insert(0, ML_DIR)
sys.path.insert(0, BTCUSD_DIR)

def get_mt5_candles(count=300):
    """Get last N M5 candles from MT5"""
    try:
        import MetaTrader5 as mt5
    except ImportError:
        return None, "MetaTrader5 module not installed"

    # Load MT5 auth
    auth_path = os.path.join(BTCUSD_DIR, "mt5_auth.json")
    if not os.path.exists(auth_path):
        return None, "mt5_auth.json not found"

    with open(auth_path, 'r') as f:
        auth = json.load(f)

    if not mt5.initialize():
        return None, f"MT5 init failed: {mt5.last_error()}"

    if not mt5.login(auth['login'], password=auth['password'], server=auth['server']):
        err = mt5.last_error()
        mt5.shutdown()
        return None, f"MT5 login failed: {err}"

    rates = mt5.copy_rates_from_pos("BTCUSD", mt5.TIMEFRAME_M5, 0, count)
    mt5.shutdown()

    if rates is None or len(rates) == 0:
        return None, "No candle data returned from MT5"

    df = pd.DataFrame(rates)
    df['time'] = pd.to_datetime(df['time'], unit='s')
    # Rename columns to match feature engineering expectations
    df = df.rename(columns={'tick_volume': 'volume'})
    return df, None


def compute_features(df):
    """Compute all features using the feature engineering module"""
    from feature_engineering import FeatureEngineering

    config_path = os.path.join(BTCUSD_DIR, "ml_config.json")
    fe = FeatureEngineering(config_path)
    df = fe.add_all_features(df)
    return df, fe.feature_names


def run_ensemble(features_dict):
    """Run ensemble prediction"""
    import joblib

    models_dir = os.path.join(BTCUSD_DIR, "models")
    config_path = os.path.join(BTCUSD_DIR, "ml_config.json")

    with open(config_path, 'r') as f:
        config = json.load(f)

    feature_names = config['features']
    paths = config['ensemble_paths']

    # Load models and scaler
    rf = joblib.load(os.path.join(BTCUSD_DIR, paths['rf_model']))
    xgb = joblib.load(os.path.join(BTCUSD_DIR, paths['xgb_model']))
    lgb = joblib.load(os.path.join(BTCUSD_DIR, paths['lgb_model']))
    scaler = joblib.load(os.path.join(BTCUSD_DIR, paths['scaler']))

    # Prepare features
    X = np.array([features_dict[f] for f in feature_names]).reshape(1, -1)
    X_scaled = scaler.transform(X)

    label_map = {0: 'SELL', 1: 'BUY', 2: 'HOLD'}

    # Get each model's prediction
    model_results = {}
    for name, model in [('rf', rf), ('xgb', xgb), ('lgb', lgb)]:
        probs = model.predict_proba(X_scaled)[0]
        pred_class = int(np.argmax(probs))
        model_results[name] = {
            'signal': label_map[pred_class],
            'confidence': float(probs[pred_class]),
            'probs': {label_map[i]: float(p) for i, p in enumerate(probs)}
        }

    # Majority vote
    from collections import Counter
    votes = [model_results[m]['signal'] for m in ['rf', 'xgb', 'lgb']]
    vote_counts = Counter(votes)
    majority_signal, majority_count = vote_counts.most_common(1)[0]

    if majority_count >= 2:
        signal = majority_signal
        # Average confidence of agreeing models
        agreeing = [model_results[m]['confidence'] for m in ['rf', 'xgb', 'lgb']
                     if model_results[m]['signal'] == signal]
        confidence = float(np.mean(agreeing))
    else:
        signal = "HOLD"
        confidence = 0.0

    return {
        "signal": signal,
        "confidence": round(confidence, 4),
        "models": {m: model_results[m]['signal'] for m in ['rf', 'xgb', 'lgb']},
        "model_confidences": {m: round(model_results[m]['confidence'], 4) for m in ['rf', 'xgb', 'lgb']}
    }


def main():
    # Suppress warnings to keep stdout clean for JSON
    import warnings
    warnings.filterwarnings('ignore')

    # Redirect prints from feature engineering to stderr
    old_stdout = sys.stdout
    sys.stdout = sys.stderr

    try:
        df, err = get_mt5_candles(300)
        if err:
            sys.stdout = old_stdout
            print(json.dumps({"error": err}))
            return

        df, feature_names = compute_features(df)

        # Get the last row's features
        last_row = df.iloc[-1]
        features_dict = {f: float(last_row[f]) for f in feature_names}

        # Check for NaN
        nan_features = [f for f, v in features_dict.items() if np.isnan(v)]
        if nan_features:
            sys.stdout = old_stdout
            print(json.dumps({"error": f"NaN in features: {nan_features}"}))
            return

        result = run_ensemble(features_dict)

        sys.stdout = old_stdout
        print(json.dumps(result))

    except Exception as e:
        sys.stdout = old_stdout
        print(json.dumps({"error": str(e)}))


if __name__ == "__main__":
    main()
