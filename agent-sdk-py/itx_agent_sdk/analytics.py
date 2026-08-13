"""Stock-market-style analytics for agents: trend %, per-capability
"sector" performance, OHLC candles, and order-book depth.

Every function here is pure -- dicts in (already-parsed JSON from
`HubClient`), dicts/lists out -- so each is unit-testable against
hand-built fixtures without a running hub, and none of them make network
calls themselves. The MCP tools in `mcp_server.py` are the thin layer
that fetches via `HubClient` and hands the response to these.

`period_change_pct` is a direct port of `periodChangePct` in
`dashboard/src/lib/series.ts` -- same algorithm, same edge cases (`None`
below two buckets or a zero-sum earlier half), so an agent's read of
"is this market heating up" agrees with what a human sees on the
dashboard. The per-capability change in `market_overview` mirrors
`sectorsFromSummary`'s market-level guard: below two *active* buckets
(i.e. at least two buckets with nonzero bounty), it reports `None`
rather than let one payout landing in one half of the window pose as a
confident +/-100%.
"""

from datetime import datetime
from typing import Any, Dict, List, Optional


def period_change_pct(series: List[float]) -> Optional[float]:
    """Change between the two halves of a bucketed series, as a
    percentage -- period-over-period, not first-point-to-last-point.
    `None` when there are fewer than two buckets, or the earlier half
    summed to zero (any activity at all from zero isn't a percentage).
    """
    if len(series) < 2:
        return None
    midpoint = len(series) // 2
    earlier = sum(series[:midpoint])
    later = sum(series[midpoint:])
    if earlier == 0:
        return None
    return (later - earlier) / earlier * 100


def capability_trend(series_dto: Dict[str, Any]) -> Dict[str, Any]:
    """Takes a `board_series()` response (one capability's, or the whole
    board's, `MarketSeriesDto`) and adds `posted_change_pct` /
    `bounty_change_pct` computed from its `posted_series` /
    `bounty_series`.
    """
    result = dict(series_dto)
    result["posted_change_pct"] = period_change_pct(series_dto.get("posted_series") or [])
    result["bounty_change_pct"] = period_change_pct(series_dto.get("bounty_series") or [])
    return result


def market_overview(summary_dto: Dict[str, Any]) -> Dict[str, Any]:
    """Takes a `board_summary()` response (`BoardSummaryDto`) and adds a
    `change_pct` to each entry in `capabilities`, computed from that
    capability's `bounty_series` -- the "sector performance" view across
    the whole board at once. Gated the same way `sectorsFromSummary`
    gates its market-level change: below two buckets with nonzero
    bounty, `change_pct` is `None` rather than a misleading spike.
    """
    result = dict(summary_dto)
    capabilities = []
    for cap in summary_dto.get("capabilities") or []:
        bounty_series = cap.get("bounty_series") or []
        active = sum(1 for v in bounty_series if v > 0)
        cap_out = dict(cap)
        cap_out["change_pct"] = period_change_pct(bounty_series) if active >= 2 else None
        capabilities.append(cap_out)
    result["capabilities"] = capabilities
    return result


def _parse_epoch_ms(rfc3339: str) -> float:
    return datetime.fromisoformat(rfc3339.replace("Z", "+00:00")).timestamp() * 1000


def price_candles(
    trades: List[Dict[str, Any]],
    interval_ms: int,
    limit: Optional[int] = None,
) -> List[Dict[str, Any]]:
    """Buckets executed trades (`TradeDto`s -- `price`, `quantity`,
    `executed_at`) into OHLCV candles of `interval_ms` width, oldest
    first. `trades` may be in any order (the hub returns them newest
    first; this sorts internally). Purpose-built for agents -- unlike
    every other analytics tool here, no frontend page shows this: the
    new dashboard has zero exchange UI.

    `limit`, if given, keeps only the most recent `limit` candles.
    """
    if interval_ms <= 0:
        raise ValueError("interval_ms must be positive")

    dated = sorted(
        (
            (_parse_epoch_ms(t["executed_at"]), t["price"], t["quantity"])
            for t in trades
        ),
        key=lambda row: row[0],
    )

    buckets: "Dict[int, Dict[str, Any]]" = {}
    for epoch_ms, price, quantity in dated:
        bucket_start = int(epoch_ms // interval_ms) * interval_ms
        candle = buckets.get(bucket_start)
        if candle is None:
            buckets[bucket_start] = {
                "bucket_start_ms": bucket_start,
                "open": price,
                "high": price,
                "low": price,
                "close": price,
                "volume": quantity,
            }
        else:
            candle["high"] = max(candle["high"], price)
            candle["low"] = min(candle["low"], price)
            candle["close"] = price
            candle["volume"] += quantity

    candles = [buckets[key] for key in sorted(buckets.keys())]
    if limit is not None:
        candles = candles[-limit:]
    return candles


def _depth_side(orders: List[Dict[str, Any]], *, best_first_descending: bool) -> List[Dict[str, Any]]:
    by_price: Dict[int, int] = {}
    for order in orders:
        remaining = order["quantity"] - order["filled"]
        if remaining <= 0:
            continue
        by_price[order["price"]] = by_price.get(order["price"], 0) + remaining

    prices = sorted(by_price.keys(), reverse=best_first_descending)
    tiers = []
    cumulative = 0
    for price in prices:
        cumulative += by_price[price]
        tiers.append({"price": price, "quantity": by_price[price], "cumulative_quantity": cumulative})
    return tiers


def market_depth(order_book: Dict[str, Any]) -> Dict[str, Any]:
    """Takes a `get_order_book()` response (`OrderBookDto` -- `bids`,
    `asks`, each a list of `OrderDto`) and returns per-price-tier depth
    (remaining, unfilled quantity only -- `quantity - filled`), best
    bid/ask, and the spread. `best_bid`/`best_ask`/`spread`/`mid_price`
    are `None` on a one-sided or empty book.
    """
    bids = _depth_side(order_book.get("bids") or [], best_first_descending=True)
    asks = _depth_side(order_book.get("asks") or [], best_first_descending=False)

    best_bid = bids[0]["price"] if bids else None
    best_ask = asks[0]["price"] if asks else None
    spread = (best_ask - best_bid) if (best_bid is not None and best_ask is not None) else None
    mid_price = ((best_ask + best_bid) / 2) if spread is not None else None

    return {
        "bids": bids,
        "asks": asks,
        "best_bid": best_bid,
        "best_ask": best_ask,
        "spread": spread,
        "mid_price": mid_price,
    }
