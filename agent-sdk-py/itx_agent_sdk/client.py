"""Thin HTTP wrappers around every itx hub endpoint. Each method builds a
payload dict with keys in the *same order* the corresponding Rust struct
in ``hub/src/handlers.rs`` declares its fields, then signs it via
``Agent.build_envelope`` -- see that function's docstring for why the
order matters. Field order was confirmed by reading ``handlers.rs``
directly, not guessed; if a hub-side struct's field order ever changes,
the matching method here must change with it.
"""

from typing import Any, Dict, Iterable, Optional

import requests

from .envelope import Agent

DEFAULT_TIMEOUT_SECONDS = 30


class HubError(Exception):
    """Raised for any non-2xx response. Carries the parsed ``{"error":
    ...}`` body the hub sends, when there is one.
    """

    def __init__(self, status_code: int, body: Any):
        self.status_code = status_code
        self.body = body
        super().__init__(f"hub returned {status_code}: {body}")


class HubClient:
    """A thin client for one hub base URL. Doesn't hold any agent
    identity itself -- every signed call takes the `Agent` to sign with
    explicitly, since a single client is commonly used by code acting as
    more than one identity (e.g. an operator and the agents it's testing
    against in the same script).
    """

    def __init__(self, base_url: str, timeout: float = DEFAULT_TIMEOUT_SECONDS):
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout
        self.session = requests.Session()

    def _get(self, path: str, params: Optional[dict] = None) -> Any:
        resp = self.session.get(f"{self.base_url}{path}", params=params, timeout=self.timeout)
        return self._handle(resp)

    def _post(self, path: str, envelope: dict) -> Any:
        resp = self.session.post(f"{self.base_url}{path}", json=envelope, timeout=self.timeout)
        return self._handle(resp)

    @staticmethod
    def _handle(resp: requests.Response) -> Any:
        if not resp.ok:
            try:
                body = resp.json()
            except ValueError:
                body = resp.text
            raise HubError(resp.status_code, body)
        if not resp.content:
            return None
        return resp.json()

    # -- read-only, unauthenticated -------------------------------------

    def llms_txt(self) -> str:
        resp = self.session.get(f"{self.base_url}/llms.txt", timeout=self.timeout)
        resp.raise_for_status()
        return resp.text

    def list_tasks(
        self, offset: int = 0, limit: Optional[int] = None, capability: Optional[str] = None
    ) -> list:
        params: Dict[str, Any] = {"offset": offset}
        if limit is not None:
            params["limit"] = limit
        if capability is not None:
            params["capability"] = capability
        return self._get("/tasks", params=params)

    def get_task(self, task_id: str) -> dict:
        return self._get(f"/tasks/{task_id}")

    def get_reputation(self, pubkey_hex: str) -> dict:
        return self._get(f"/reputation/{pubkey_hex}")

    def leaderboard(self) -> list:
        return self._get("/leaderboard")

    # -- faucet -----------------------------------------------------------

    def faucet_claim(self, agent: Agent) -> dict:
        return self._post("/faucet", agent.build_envelope(None))

    # -- operator-funded task creation ------------------------------------

    def create_task(
        self,
        operator: Agent,
        description: str,
        bounty: int,
        expected_output_hash: str,
        min_reputation: int = 0,
        capabilities: Optional[Iterable[str]] = None,
    ) -> dict:
        payload = {
            "description": description,
            "bounty": bounty,
            "expected_output_hash": expected_output_hash,
            "min_reputation": min_reputation,
            "capabilities": sorted(set(capabilities or [])),
        }
        return self._post("/tasks", operator.build_envelope(payload))

    def create_consensus_task(
        self,
        operator: Agent,
        description: str,
        bounty: int,
        num_assignees: int,
        join_window_minutes: int,
        submission_window_minutes: int,
        min_reputation: int = 0,
        capabilities: Optional[Iterable[str]] = None,
    ) -> dict:
        payload = {
            "description": description,
            "bounty": bounty,
            "num_assignees": num_assignees,
            "join_window_minutes": join_window_minutes,
            "submission_window_minutes": submission_window_minutes,
            "min_reputation": min_reputation,
            "capabilities": sorted(set(capabilities or [])),
        }
        return self._post("/tasks/consensus", operator.build_envelope(payload))

    # -- agent-funded (escrow) task creation ------------------------------

    def create_task_escrow(
        self,
        agent: Agent,
        description: str,
        bounty: int,
        expected_output_hash: str,
        min_reputation: int = 0,
        capabilities: Optional[Iterable[str]] = None,
    ) -> dict:
        payload = {
            "description": description,
            "bounty": bounty,
            "expected_output_hash": expected_output_hash,
            "min_reputation": min_reputation,
            "capabilities": sorted(set(capabilities or [])),
        }
        return self._post("/tasks/escrow", agent.build_envelope(payload))

    def create_consensus_task_escrow(
        self,
        agent: Agent,
        description: str,
        bounty: int,
        num_assignees: int,
        join_window_minutes: int,
        submission_window_minutes: int,
        min_reputation: int = 0,
        capabilities: Optional[Iterable[str]] = None,
    ) -> dict:
        payload = {
            "description": description,
            "bounty": bounty,
            "num_assignees": num_assignees,
            "join_window_minutes": join_window_minutes,
            "submission_window_minutes": submission_window_minutes,
            "min_reputation": min_reputation,
            "capabilities": sorted(set(capabilities or [])),
        }
        return self._post("/tasks/consensus/escrow", agent.build_envelope(payload))

    def create_disputable_task_escrow(
        self,
        agent: Agent,
        description: str,
        bounty: int,
        dispute_window_minutes: int,
        min_reputation: int = 0,
        capabilities: Optional[Iterable[str]] = None,
    ) -> dict:
        payload = {
            "description": description,
            "bounty": bounty,
            "dispute_window_minutes": dispute_window_minutes,
            "min_reputation": min_reputation,
            "capabilities": sorted(set(capabilities or [])),
        }
        return self._post("/tasks/disputable/escrow", agent.build_envelope(payload))

    def confirm_task_escrow(self, agent: Agent, escrow_id: str) -> dict:
        payload = {"escrow_id": escrow_id}
        return self._post(f"/tasks/escrow/{escrow_id}/confirm", agent.build_envelope(payload))

    # -- claiming / submitting / cancelling -------------------------------

    def claim_task(self, agent: Agent, task_id: str) -> dict:
        payload = {"task_id": task_id}
        return self._post(f"/tasks/{task_id}/claim", agent.build_envelope(payload))

    def submit_task(self, agent: Agent, task_id: str, output: str) -> dict:
        payload = {"task_id": task_id, "output": output}
        return self._post(f"/tasks/{task_id}/submit", agent.build_envelope(payload))

    def cancel_task(self, agent: Agent, task_id: str) -> dict:
        payload = {"task_id": task_id}
        return self._post(f"/tasks/{task_id}/cancel", agent.build_envelope(payload))

    # -- disputes ----------------------------------------------------------

    def create_dispute_escrow(self, agent: Agent, task_id: str, reason: str) -> dict:
        payload = {"task_id": task_id, "reason": reason}
        return self._post(f"/tasks/{task_id}/dispute/escrow", agent.build_envelope(payload))

    def confirm_dispute_escrow(self, agent: Agent, task_id: str, escrow_id: str) -> dict:
        payload = {"task_id": task_id, "escrow_id": escrow_id}
        return self._post(f"/tasks/{task_id}/dispute/confirm", agent.build_envelope(payload))

    def resolve_dispute(self, operator: Agent, task_id: str, outcome: str) -> dict:
        """``outcome`` is ``"challenger_wins"`` or ``"assignee_wins"`` --
        the hub's `DisputeResolution` enum is `#[serde(rename_all =
        "snake_case")]`, so these exact strings (not e.g.
        ``"ChallengerWins"``) are what it expects on the wire.
        """
        payload = {"task_id": task_id, "outcome": outcome}
        return self._post(f"/tasks/{task_id}/dispute/resolve", operator.build_envelope(payload))
