"""Unit tests for the pure analytics functions in `analytics.py` --
hand-built fixtures with known expected output, no hub involved. See
that module's docstring for why each algorithm is shaped the way it is.
"""

from itx_agent_sdk.analytics import (
    capability_trend,
    market_depth,
    market_overview,
    period_change_pct,
    price_candles,
)


# -- period_change_pct -------------------------------------------------


def test_period_change_pct_none_below_two_buckets():
    assert period_change_pct([]) is None
    assert period_change_pct([5]) is None


def test_period_change_pct_none_when_earlier_half_is_zero():
    assert period_change_pct([0, 0, 5, 5]) is None


def test_period_change_pct_computes_period_over_period_increase():
    # earlier = 2+2 = 4, later = 6+6 = 12 -> (12-4)/4*100 = 200
    assert period_change_pct([2, 2, 6, 6]) == 200.0


def test_period_change_pct_computes_period_over_period_decrease():
    # earlier = 10+10=20, later = 5+5=10 -> (10-20)/20*100 = -50
    assert period_change_pct([10, 10, 5, 5]) == -50.0


def test_period_change_pct_odd_length_splits_by_floor_midpoint():
    # midpoint = 5 // 2 = 2; earlier = series[:2] = [1,1]=2, later = series[2:] = [1,1,10]=12
    assert period_change_pct([1, 1, 1, 1, 10]) == 500.0


# -- capability_trend ----------------------------------------------------


def test_capability_trend_adds_posted_and_bounty_change_pct():
    series_dto = {
        "capability": "python",
        "window_ms": 3600000,
        "buckets": 4,
        "posted_series": [1, 1, 3, 3],
        "bounty_series": [100, 100, 50, 50],
    }
    result = capability_trend(series_dto)
    assert result["posted_change_pct"] == 200.0
    assert result["bounty_change_pct"] == -50.0
    # original fields preserved
    assert result["capability"] == "python"
    assert result["buckets"] == 4


def test_capability_trend_handles_missing_series_gracefully():
    result = capability_trend({"capability": "python"})
    assert result["posted_change_pct"] is None
    assert result["bounty_change_pct"] is None


# -- market_overview -------------------------------------------------------


def test_market_overview_adds_change_pct_per_capability():
    summary_dto = {
        "window_ms": 3600000,
        "buckets": 4,
        "capabilities": [
            {
                "capability": "python",
                "open": 3,
                "open_bounty": 300,
                "posted": 10,
                "posted_series": [1, 2, 3, 4],
                "bounty_series": [100, 100, 50, 50],
            },
            {
                "capability": "rust",
                "open": 1,
                "open_bounty": 50,
                "posted": 1,
                # only one active bucket -- below the active>=2 gate
                "bounty_series": [0, 0, 0, 50],
            },
        ],
    }
    result = market_overview(summary_dto)
    caps = {c["capability"]: c for c in result["capabilities"]}
    assert caps["python"]["change_pct"] == -50.0
    assert caps["rust"]["change_pct"] is None
    # untouched fields still present
    assert caps["python"]["open_bounty"] == 300


def test_market_overview_leaves_capabilities_with_no_activity_at_none():
    summary_dto = {"capabilities": [{"capability": "empty", "bounty_series": [0, 0, 0, 0]}]}
    result = market_overview(summary_dto)
    assert result["capabilities"][0]["change_pct"] is None


# -- price_candles -----------------------------------------------------


def _trade(price, quantity, executed_at):
    return {"price": price, "quantity": quantity, "executed_at": executed_at}


def test_price_candles_buckets_and_computes_ohlcv():
    trades = [
        # newest first, as list_trades returns them -- price_candles must
        # sort internally rather than assume order.
        _trade(120, 2, "2024-01-01T00:01:30Z"),
        _trade(110, 1, "2024-01-01T00:00:30Z"),
        _trade(100, 5, "2024-01-01T00:00:00Z"),
    ]
    candles = price_candles(trades, interval_ms=60_000)
    assert len(candles) == 2

    first, second = candles
    assert first["bucket_start_ms"] == 1704067200000  # 2024-01-01T00:00:00Z
    assert first["open"] == 100
    assert first["high"] == 110
    assert first["low"] == 100
    assert first["close"] == 110
    assert first["volume"] == 6

    assert second["bucket_start_ms"] == 1704067260000  # 2024-01-01T00:01:00Z
    assert second["open"] == 120
    assert second["high"] == 120
    assert second["low"] == 120
    assert second["close"] == 120
    assert second["volume"] == 2


def test_price_candles_empty_trades_returns_empty_list():
    assert price_candles([], interval_ms=60_000) == []


def test_price_candles_respects_limit_keeping_the_most_recent():
    trades = [
        _trade(100, 1, "2024-01-01T00:00:00Z"),
        _trade(101, 1, "2024-01-01T00:01:00Z"),
        _trade(102, 1, "2024-01-01T00:02:00Z"),
    ]
    candles = price_candles(trades, interval_ms=60_000, limit=2)
    assert len(candles) == 2
    assert candles[0]["open"] == 101
    assert candles[1]["open"] == 102


def test_price_candles_rejects_non_positive_interval():
    import pytest

    with pytest.raises(ValueError):
        price_candles([], interval_ms=0)


# -- market_depth --------------------------------------------------------


def _order(side, price, quantity, filled=0):
    return {"side": side, "price": price, "quantity": quantity, "filled": filled}


def test_market_depth_computes_best_bid_ask_and_spread():
    order_book = {
        "bids": [_order("buy", 95, 10), _order("buy", 100, 5), _order("buy", 90, 20)],
        "asks": [_order("sell", 110, 8), _order("sell", 105, 3)],
    }
    depth = market_depth(order_book)

    assert depth["best_bid"] == 100
    assert depth["best_ask"] == 105
    assert depth["spread"] == 5
    assert depth["mid_price"] == 102.5

    # bids ordered best (highest) first, cumulative quantity accumulates
    assert [tier["price"] for tier in depth["bids"]] == [100, 95, 90]
    assert depth["bids"][0]["cumulative_quantity"] == 5
    assert depth["bids"][1]["cumulative_quantity"] == 15
    assert depth["bids"][2]["cumulative_quantity"] == 35

    # asks ordered best (lowest) first
    assert [tier["price"] for tier in depth["asks"]] == [105, 110]
    assert depth["asks"][0]["cumulative_quantity"] == 3
    assert depth["asks"][1]["cumulative_quantity"] == 11


def test_market_depth_merges_orders_at_the_same_price_tier():
    order_book = {"bids": [_order("buy", 100, 5), _order("buy", 100, 3)], "asks": []}
    depth = market_depth(order_book)
    assert len(depth["bids"]) == 1
    assert depth["bids"][0]["quantity"] == 8


def test_market_depth_excludes_fully_filled_orders():
    order_book = {"bids": [_order("buy", 100, 5, filled=5)], "asks": []}
    depth = market_depth(order_book)
    assert depth["bids"] == []
    assert depth["best_bid"] is None


def test_market_depth_none_on_one_sided_book():
    order_book = {"bids": [_order("buy", 100, 5)], "asks": []}
    depth = market_depth(order_book)
    assert depth["best_ask"] is None
    assert depth["spread"] is None
    assert depth["mid_price"] is None


def test_market_depth_empty_book():
    depth = market_depth({"bids": [], "asks": []})
    assert depth == {
        "bids": [],
        "asks": [],
        "best_bid": None,
        "best_ask": None,
        "spread": None,
        "mid_price": None,
    }
