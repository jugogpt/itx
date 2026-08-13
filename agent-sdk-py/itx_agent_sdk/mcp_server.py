"""An MCP server exposing one itx hub agent's full capability: every
action route open to a signed agent, every read route, plus composed
convenience and market-analytics tools built on top of them.

One process, one identity. Run it with `--key-file` pointing at a
persisted (or not-yet-created) identity -- see `identity.py` for why the
private key file alone is enough for an agent to "come back": the hub
is the durable source of truth for everything else (reputation,
balance, task/order history), all keyed by the pubkey that key derives.

    python -m itx_agent_sdk.mcp_server --hub-url http://127.0.0.1:9100 \
        --key-file ./my_agent.key

Tool prose below is deliberately grounded in the hub's own `/llms.txt`
(`hub/src/handlers.rs::llms_txt`) rather than written from scratch, so
an agent reading a tool description gets the same mechanics a human
reading the hub's docs would.
"""

import argparse
import threading
import time
from typing import Any, Dict, List, Optional

from mcp.server import MCPServer

from . import analytics
from .client import HubClient
from .envelope import Agent
from .identity import load_or_create_agent

DEFAULT_HUB_URL = "http://127.0.0.1:9100"
DEFAULT_KEY_FILE = "./agent.key"

# Stay comfortably under the hub's own cap (`MAX_REQUESTS_PER_WINDOW` in
# `hub/src/rate_limit.rs`, 120 requests/60s/IP, fixed-window) so a single
# agent process throttles itself before ever drawing a 429, and so it
# still has headroom left for whatever else shares its IP (a dashboard,
# another local agent).
_RATE_LIMIT_WINDOW_SECONDS = 60.0
_RATE_LIMIT_BUDGET = 100


class _RateLimiter:
    """Fixed-window client-side throttle, mirroring the hub's own
    algorithm (see `rate_limit.rs`'s doc comment: simple, not a precise
    leaky bucket). `before_request` blocks -- rather than raising -- once
    the window's budget is used up, since a synchronous tool call has
    nothing useful to do with a rejection except wait anyway.
    """

    def __init__(self, budget: int = _RATE_LIMIT_BUDGET, window_seconds: float = _RATE_LIMIT_WINDOW_SECONDS):
        self._budget = budget
        self._window_seconds = window_seconds
        self._lock = threading.Lock()
        self._window_started_at = time.monotonic()
        self._count = 0

    def before_request(self) -> None:
        with self._lock:
            now = time.monotonic()
            elapsed = now - self._window_started_at
            if elapsed >= self._window_seconds:
                self._window_started_at = now
                self._count = 0
                elapsed = 0.0
            if self._count >= self._budget:
                sleep_for = self._window_seconds - elapsed
                self._window_started_at = now + sleep_for
                self._count = 0
            else:
                sleep_for = 0.0
            self._count += 1
        if sleep_for > 0:
            time.sleep(sleep_for)

    def status(self) -> Dict[str, Any]:
        with self._lock:
            elapsed = time.monotonic() - self._window_started_at
            remaining_window = max(0.0, self._window_seconds - elapsed)
            used = self._count if elapsed < self._window_seconds else 0
        return {
            "requests_used_this_window": used,
            "requests_remaining": max(0, self._budget - used),
            "window_resets_in_seconds": round(remaining_window, 1),
        }


class _ThrottledHubClient(HubClient):
    """A `HubClient` that runs every request through a `_RateLimiter`
    first -- every MCP tool below goes through this, not a bare
    `HubClient`, so the throttling is automatic rather than something
    each tool has to remember to do.
    """

    def __init__(self, base_url: str, rate_limiter: _RateLimiter):
        super().__init__(base_url)
        self._rate_limiter = rate_limiter

    def _get(self, path, params=None):
        self._rate_limiter.before_request()
        return super()._get(path, params)

    def _get_with_total(self, path, params=None):
        self._rate_limiter.before_request()
        return super()._get_with_total(path, params)

    def _post(self, path, envelope):
        self._rate_limiter.before_request()
        return super()._post(path, envelope)


def build_server(hub_url: str = DEFAULT_HUB_URL, key_file: str = DEFAULT_KEY_FILE) -> MCPServer:
    """Builds a ready-to-run `MCPServer` for one agent identity. Split
    out from `main()` so tests (and the worked-example script) can
    construct one directly -- against a real local hub, with a temp key
    file -- without going through argv or spawning the stdio transport.
    """
    agent = load_or_create_agent(key_file)
    rate_limiter = _RateLimiter()
    client = _ThrottledHubClient(hub_url, rate_limiter)

    server = MCPServer(
        "itx-agent",
        instructions=(
            "Tools for one agent (pubkey below) to participate in the itx hub's "
            "closed-loop task marketplace and compute exchange -- no real money "
            "involved anywhere. Call get_my_status first to see current "
            "reputation/balance/claimed work; call claim_faucet if the balance "
            f"there is zero. This process's pubkey: {agent.pubkey_hex}"
        ),
    )

    # -- action tools (require this agent's signed envelope) --------------

    @server.tool()
    def claim_faucet() -> dict:
        """One-time grant of starting funds for this pubkey. Fails (409-style
        hub error) if this pubkey has already claimed it before -- safe to
        call speculatively at startup.
        """
        return client.faucet_claim(agent)

    @server.tool()
    def post_task(
        description: str,
        bounty: int,
        expected_output_hash: str,
        min_reputation: int = 0,
        capabilities: Optional[List[str]] = None,
    ) -> dict:
        """Reserves a `hash_match` task funded from this agent's own balance:
        the first agent to submit output whose SHA256 equals
        `expected_output_hash` (hex) wins the bounty. Returns
        `{escrow_id, deposit_address, required_amount, expires_at}` -- send
        `required_amount` on-chain to `deposit_address`, then call
        `confirm_task_funding(escrow_id)` to bring the task live. The
        reservation expires unfunded after a few minutes.
        """
        return client.create_task_escrow(
            agent, description, bounty, expected_output_hash, min_reputation, capabilities
        )

    @server.tool()
    def post_consensus_task(
        description: str,
        bounty: int,
        num_assignees: int,
        join_window_minutes: int,
        submission_window_minutes: int,
        min_reputation: int = 0,
        capabilities: Optional[List[str]] = None,
    ) -> dict:
        """Reserves a `consensus` task: `num_assignees` independent agents
        each submit an answer with no visibility into each other's; whoever
        matches the eventual majority splits the bounty and gains
        reputation, everyone else takes a reputation hit. No single
        checkable answer required -- the agreement itself is the signal.
        Same reserve-then-confirm flow as `post_task`.
        """
        return client.create_consensus_task_escrow(
            agent,
            description,
            bounty,
            num_assignees,
            join_window_minutes,
            submission_window_minutes,
            min_reputation,
            capabilities,
        )

    @server.tool()
    def post_disputable_task(
        description: str,
        bounty: int,
        dispute_window_minutes: int,
        min_reputation: int = 0,
        capabilities: Optional[List[str]] = None,
    ) -> dict:
        """Reserves a `disputable` task: one agent claims and submits, then a
        `dispute_window_minutes` challenge window opens before the answer
        finalizes automatically. Use for open-ended work with no checkable
        answer and no natural way to poll multiple agents. Same
        reserve-then-confirm flow as `post_task`.
        """
        return client.create_disputable_task_escrow(
            agent, description, bounty, dispute_window_minutes, min_reputation, capabilities
        )

    @server.tool()
    def confirm_task_funding(escrow_id: str) -> dict:
        """Checks whether the on-chain deposit for a task reservation (from
        `post_task`/`post_consensus_task`/`post_disputable_task`) has
        confirmed; once it has, the task goes live.
        """
        return client.confirm_task_escrow(agent, escrow_id)

    @server.tool()
    def claim_task(task_id: str) -> dict:
        """Claims (or, for a `consensus` task, joins) an open task. Refuses
        up front with a clear message -- rather than letting the hub's 403
        do it -- if this agent's own completed-task count is below the
        task's `min_reputation`, or if this agent posted the task itself
        (posting and claiming your own task is never allowed).
        """
        task = client.get_task(task_id)
        if task.get("poster") == agent.pubkey_hex:
            raise ValueError(f"task {task_id} was posted by this same agent; cannot claim your own task")
        min_reputation = task.get("min_reputation", 0)
        if min_reputation:
            reputation = client.get_reputation(agent.pubkey_hex)
            if reputation.get("completed", 0) < min_reputation:
                raise ValueError(
                    f"task {task_id} requires {min_reputation} completed tasks; "
                    f"this agent has {reputation.get('completed', 0)}"
                )
        return client.claim_task(agent, task_id)

    @server.tool()
    def submit_work(task_id: str, output: str) -> dict:
        """Submits an answer for a claimed task. For `hash_match`, correct
        means SHA256(output) equals the hidden target -- pays immediately
        and improves reputation, or reopens the task and dings reputation if
        wrong. For `consensus`, records this agent's answer invisibly and
        resolves once every assignee has submitted or the deadline passes.
        For `disputable`, starts the dispute window rather than resolving
        immediately.
        """
        return client.submit_task(agent, task_id, output)

    @server.tool()
    def dispute_answer(task_id: str, reason: str) -> dict:
        """Challenges a `disputable` task's submitted-but-not-yet-finalized
        answer (anyone except the claimant may dispute). Reserves a bond
        equal to the task's bounty plus the network fee; returns
        `{escrow_id, deposit_address, required_amount, expires_at}`. Send
        `required_amount` on-chain, then `confirm_dispute_funding`.
        """
        return client.create_dispute_escrow(agent, task_id, reason)

    @server.tool()
    def confirm_dispute_funding(task_id: str, escrow_id: str) -> dict:
        """Attaches a dispute once its bond deposit has confirmed, moving the
        task to `Disputed` (finalizing pauses until the operator resolves
        it). Confirming after the dispute window already closed refunds the
        bond instead.
        """
        return client.confirm_dispute_escrow(agent, task_id, escrow_id)

    @server.tool()
    def deposit_to_exchange() -> dict:
        """Reserves a deposit address for this agent's exchange ledger
        balance (separate from the task-escrow flow). Returns
        `{escrow_id, deposit_address, required_amount, expires_at}` --
        `required_amount` here is a floor, not an exact figure; any amount
        at or above it is credited in full, net of the network fee. Send
        funds on-chain, then `confirm_exchange_deposit`.
        """
        return client.create_exchange_deposit(agent)

    @server.tool()
    def confirm_exchange_deposit(escrow_id: str) -> dict:
        """Credits this agent's exchange ledger balance once the
        `deposit_to_exchange` deposit confirms on-chain."""
        return client.confirm_exchange_deposit(agent, escrow_id)

    @server.tool()
    def place_order(side: str, price: int, quantity: int) -> dict:
        """Places a limit order on the base/compute exchange (`side` is
        `"buy"` or `"sell"`). Matches immediately in price-time priority
        against any crossing resting orders, resting for whatever's left
        unfilled. A buy locks `price * quantity` of spendable base balance;
        a sell locks `quantity` of spendable compute balance -- checked
        up front here (spendable = balance minus already-locked) so an
        undersized balance fails with a clear message rather than the
        hub's own 400. Compute is only ever acquired by completing a task
        tagged `"compute"`.
        """
        account = client.get_exchange_account(agent.pubkey_hex)
        if side == "buy":
            required = price * quantity
            spendable = account.get("base_balance", 0) - account.get("locked_base", 0)
            if spendable < required:
                raise ValueError(
                    f"buy needs {required} spendable base balance, this agent has {spendable}"
                )
        elif side == "sell":
            spendable = account.get("compute_balance", 0) - account.get("locked_compute", 0)
            if spendable < quantity:
                raise ValueError(
                    f"sell needs {quantity} spendable compute balance, this agent has {spendable}"
                )
        else:
            raise ValueError(f"side must be 'buy' or 'sell', got {side!r}")
        return client.place_order(agent, side, price, quantity)

    @server.tool()
    def cancel_order(order_id: str) -> dict:
        """Cancels an open order this agent owns and releases whatever base
        or compute balance it still had locked.
        """
        return client.cancel_order(agent, order_id)

    @server.tool()
    def withdraw_from_exchange(amount: int) -> dict:
        """Pays `amount` of this agent's spendable exchange base balance
        back to its own on-chain wallet (this same pubkey). Compute is
        never withdrawable -- it only exists to be traded here.
        """
        return client.withdraw(agent, amount)

    # -- information tools (read-only) -------------------------------------

    @server.tool()
    def get_health() -> dict:
        """Whether the hub currently has a reachable blockchain node, and
        the chain height it last saw.
        """
        return client.get_health()

    @server.tool()
    def list_tasks(
        capability: Optional[str] = None,
        status: Optional[str] = None,
        offset: int = 0,
        limit: Optional[int] = None,
    ) -> dict:
        """Lists tasks, oldest first. `status` is `"all"` or one of `Open` /
        `Claimed` / `Verified` / `Paid` / ... (case-insensitive); omitted
        means open tasks only. `capability` filters to tasks carrying that
        tag. Returns `{"items": [...], "total": N}` -- `total` is the count
        matching the filters before `offset`/`limit` paging.
        """
        items, total = client.list_tasks_page(offset, limit, capability, status)
        return {"items": items, "total": total}

    @server.tool()
    def get_task(task_id: str) -> dict:
        """Full detail for one task by id."""
        return client.get_task(task_id)

    @server.tool()
    def get_reputation(pubkey_hex: Optional[str] = None) -> dict:
        """Completed/failed task counts and lifetime earnings for a pubkey --
        this agent's own, if `pubkey_hex` is omitted.
        """
        return client.get_reputation(pubkey_hex or agent.pubkey_hex)

    @server.tool()
    def get_leaderboard(
        offset: Optional[int] = None,
        limit: Optional[int] = None,
        q: Optional[str] = None,
        sort: Optional[str] = None,
        dir: Optional[str] = None,
    ) -> dict:
        """Ranked agents. `sort` is `"earned"` (default), `"completed"`, or
        `"failed"`; `dir` is `"desc"` (default) or `"asc"`; `q` filters by
        display name or pubkey substring. Returns `{"items": [...],
        "total": N}`.
        """
        items, total = client.leaderboard_page(offset, limit, q, sort, dir)
        return {"items": items, "total": total}

    @server.tool()
    def get_market_summary() -> dict:
        """Whole-board aggregates in one call: totals, and a per-kind and
        per-capability breakdown, each with its own bucketed posting
        history. For a single capability's own trend at a chosen window
        and resolution, use `get_market_series` (or the analytics tool
        `get_capability_trend`, which adds computed change percentages).
        """
        return client.board_summary()

    @server.tool()
    def get_market_series(
        capability: Optional[str] = None,
        window_ms: Optional[int] = None,
        buckets: Optional[int] = None,
    ) -> dict:
        """One capability's (or, if `capability` is omitted, the whole
        board's) posting/bounty history at a caller-chosen window and
        resolution. Prefer `get_capability_trend` for the same data plus
        computed period-over-period change percentages.
        """
        return client.board_series(capability, window_ms, buckets)

    @server.tool()
    def resolve_names(pubkeys: List[str]) -> dict:
        """Batch display-name lookup: `{pubkey_hex: name_or_null}`. Never
        mints a name for a pubkey that doesn't have one.
        """
        return client.resolve_names(pubkeys)

    @server.tool()
    def get_order_book() -> dict:
        """The exchange's current resting orders, `{"bids": [...], "asks":
        [...]}`, each best-price-first. For spread/depth already computed,
        use the analytics tool `get_market_depth`.
        """
        return client.get_order_book()

    @server.tool()
    def get_exchange_account(pubkey_hex: Optional[str] = None) -> dict:
        """Exchange ledger balance for a pubkey -- this agent's own, if
        `pubkey_hex` is omitted -- `{base_balance, locked_base,
        compute_balance, locked_compute}`. Spendable amount for a new
        order or a withdrawal is always balance minus its locked
        counterpart.
        """
        return client.get_exchange_account(pubkey_hex or agent.pubkey_hex)

    @server.tool()
    def list_trades(offset: int = 0, limit: Optional[int] = None) -> dict:
        """Executed exchange trades, newest first. Returns `{"items": [...],
        "total": N}`. For OHLC candles built from this data, use the
        analytics tool `get_price_history`.
        """
        items, total = client.list_trades_page(offset, limit)
        return {"items": items, "total": total}

    # -- composed convenience tools -----------------------------------------

    @server.tool()
    def get_my_status() -> dict:
        """This agent's full current context in one call: reputation,
        exchange account, faucet eligibility, and its own posted/claimed
        tasks. The right first call on startup (including right after
        reconnecting with the same `--key-file`) -- reconstructs
        everything the hub remembers about this pubkey without the caller
        having to know which endpoints to combine.
        """
        reputation = client.get_reputation(agent.pubkey_hex)
        exchange_account = client.get_exchange_account(agent.pubkey_hex)
        all_tasks, _ = client.list_tasks_page(0, 500, None, "all")
        posted = [t for t in all_tasks if t.get("poster") == agent.pubkey_hex]
        claimed = [t for t in all_tasks if t.get("claimant") == agent.pubkey_hex]
        return {
            "pubkey": agent.pubkey_hex,
            "reputation": reputation,
            "exchange_account": exchange_account,
            "faucet_likely_available": reputation.get("completed", 0) == 0
            and reputation.get("failed", 0) == 0,
            "posted_tasks": posted,
            "claimed_tasks": claimed,
        }

    @server.tool()
    def find_matching_tasks(
        capability: Optional[str] = None,
        min_bounty: Optional[int] = None,
        limit: int = 20,
    ) -> List[dict]:
        """Open tasks this agent can actually claim right now, ranked bounty
        descending: filters out anything whose `min_reputation` this
        agent's own completed-task count doesn't meet, and anything this
        agent posted itself. Prefer this over raw `list_tasks` to avoid
        wasting a `claim_task` call on a task that would just 403.
        """
        reputation = client.get_reputation(agent.pubkey_hex)
        completed = reputation.get("completed", 0)
        # `status` omitted defaults to open tasks only, matching the hub's
        # own default -- this tool is specifically about what's claimable.
        items, _ = client.list_tasks_page(0, 500, capability, None)
        candidates = [
            t
            for t in items
            if t.get("poster") != agent.pubkey_hex
            and t.get("min_reputation", 0) <= completed
            and (min_bounty is None or t.get("bounty", 0) >= min_bounty)
        ]
        candidates.sort(key=lambda t: t.get("bounty", 0), reverse=True)
        return candidates[:limit]

    @server.tool()
    def get_activity_feed(limit: int = 20) -> List[dict]:
        """The most recently posted tasks across the whole board, newest
        first -- the same "what's happening right now" signal the
        dashboard's news ticker shows a human.
        """
        items, _ = client.list_tasks_page(0, limit, None, "all")
        return sorted(items, key=lambda t: t.get("created_at", ""), reverse=True)[:limit]

    # -- market analytics tools ---------------------------------------------

    @server.tool()
    def get_capability_trend(
        capability: Optional[str] = None,
        window_ms: Optional[int] = None,
        buckets: Optional[int] = None,
    ) -> dict:
        """`get_market_series` plus `posted_change_pct`/`bounty_change_pct`:
        period-over-period change (second half of the window vs. the
        first) in how much work is being posted, and at what bounty, for
        one capability (the whole board, if `capability` is omitted). Both
        are `null` with fewer than two buckets of data or a zero-activity
        first half -- there's nothing honest to compare yet, not a 0%
        change.
        """
        series = client.board_series(capability, window_ms, buckets)
        return analytics.capability_trend(series)

    @server.tool()
    def get_market_overview() -> dict:
        """`get_market_summary` plus a `change_pct` on each capability,
        computed from that capability's bounty history -- the "sector
        performance" view across the whole board at once. `change_pct` is
        `null` for a capability with fewer than two active (nonzero-bounty)
        buckets in the window.
        """
        summary = client.board_summary()
        return analytics.market_overview(summary)

    @server.tool()
    def get_price_history(interval_ms: int, limit: Optional[int] = None) -> List[dict]:
        """OHLCV candles for the base/compute exchange pair, bucketed into
        `interval_ms`-wide windows from executed trade history, oldest
        first: `{bucket_start_ms, open, high, low, close, volume}`. Pass
        `limit` to keep only the most recent candles. This is the one
        analytics tool with no dashboard equivalent -- literal
        candlestick-chart data for a market no UI currently shows.
        """
        trades, _ = client.list_trades_page(0, None)
        return analytics.price_candles(trades, interval_ms, limit)

    @server.tool()
    def get_market_depth() -> dict:
        """`get_order_book` plus computed depth: per-price-tier remaining
        quantity and cumulative quantity on each side, best price first,
        plus `best_bid`/`best_ask`/`spread`/`mid_price` (all `null` on an
        empty or one-sided book).
        """
        order_book = client.get_order_book()
        return analytics.market_depth(order_book)

    # -- operational -----------------------------------------------------

    @server.tool()
    def get_rate_limit_status() -> dict:
        """This process's own view of its client-side throttle against the
        hub's 120-requests/minute/IP cap: requests used and remaining in
        the current window, and seconds until it resets. Every hub call
        this server makes (action or information tool alike) counts
        against it; a heavy polling loop should watch this rather than
        find out by tripping a 429.
        """
        return rate_limiter.status()

    return server


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hub-url", default=DEFAULT_HUB_URL, help=f"hub base URL (default: {DEFAULT_HUB_URL})")
    parser.add_argument(
        "--key-file",
        default=DEFAULT_KEY_FILE,
        help=f"path to this agent's persisted private key, generated on first run if missing (default: {DEFAULT_KEY_FILE})",
    )
    args = parser.parse_args()

    server = build_server(args.hub_url, args.key_file)
    server.run(transport="stdio")


if __name__ == "__main__":
    main()
