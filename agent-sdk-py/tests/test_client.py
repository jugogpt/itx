"""Unit tests for `HubClient`'s request construction -- verifies each
wrapper method hits the right URL with the right payload shape, without
needing a live hub. Mocks at the `requests.Session` level (rather than
patching `HubClient._get`/`_post` themselves) so the real `_handle`
response-parsing logic is exercised too, not bypassed.

The signing itself is already exhaustively covered against the real Rust
implementation by `test_envelope_conformance.py`; these tests are only
about whether each method sends the envelope to the right place with the
right payload.
"""

from unittest.mock import ANY, MagicMock

import pytest

from itx_agent_sdk import Agent, HubClient, HubError


def make_client_with_mock_session() -> HubClient:
    client = HubClient("http://hub.test")
    client.session = MagicMock()
    return client


def mock_response(json_body=None, status_code: int = 200) -> MagicMock:
    resp = MagicMock()
    resp.ok = 200 <= status_code < 300
    resp.status_code = status_code
    resp.content = b"{}" if json_body is not None else b""
    resp.json.return_value = json_body
    resp.text = "" if json_body is None else str(json_body)
    return resp


def test_list_tasks_hits_the_right_url_and_params():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response([])
    client.list_tasks(offset=5, limit=10, capability="python")
    client.session.get.assert_called_once_with(
        "http://hub.test/tasks",
        params={"offset": 5, "limit": 10, "capability": "python"},
        timeout=ANY,
    )


def test_list_tasks_omits_absent_optional_params():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response([])
    client.list_tasks()
    client.session.get.assert_called_once_with(
        "http://hub.test/tasks", params={"offset": 0}, timeout=ANY
    )


def test_get_task_hits_the_right_url():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response({"id": "abc"})
    result = client.get_task("abc")
    client.session.get.assert_called_once_with(
        "http://hub.test/tasks/abc", params=None, timeout=ANY
    )
    assert result == {"id": "abc"}


def test_leaderboard_and_reputation_urls():
    client = make_client_with_mock_session()
    client.session.get.return_value = mock_response([])
    client.leaderboard()
    client.session.get.assert_called_with("http://hub.test/leaderboard", params=None, timeout=ANY)

    client.get_reputation("02aabbcc")
    client.session.get.assert_called_with(
        "http://hub.test/reputation/02aabbcc", params=None, timeout=ANY
    )


def test_faucet_claim_signs_a_null_payload():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"amount": 50_000_000})
    agent = Agent.generate()

    client.faucet_claim(agent)

    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/faucet"
    envelope = kwargs["json"]
    assert envelope["pubkey"] == agent.pubkey_hex
    assert envelope["payload"] is None


def test_create_task_sends_fields_in_struct_declaration_order():
    """Field order matters here -- see `Agent.build_envelope`'s
    docstring. `hub/src/handlers.rs::CreateTaskPayload` declares its
    fields as description, bounty, expected_output_hash, min_reputation,
    capabilities, in that order.
    """
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"id": "abc"})
    operator = Agent.generate()

    client.create_task(operator, "desc", 1000, "deadbeef", min_reputation=2, capabilities=["b", "a"])

    _, kwargs = client.session.post.call_args
    payload = kwargs["json"]["payload"]
    assert list(payload.keys()) == [
        "description",
        "bounty",
        "expected_output_hash",
        "min_reputation",
        "capabilities",
    ]
    assert payload["capabilities"] == ["a", "b"], "capabilities are sorted, matching a BTreeSet's own ordering"


def test_create_consensus_task_escrow_url_and_payload():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"escrow_id": "e1"})
    agent = Agent.generate()

    client.create_consensus_task_escrow(agent, "desc", 900, 3, 30, 30)

    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/tasks/consensus/escrow"
    payload = kwargs["json"]["payload"]
    assert list(payload.keys()) == [
        "description",
        "bounty",
        "num_assignees",
        "join_window_minutes",
        "submission_window_minutes",
        "min_reputation",
        "capabilities",
    ]


def test_confirm_task_escrow_uses_escrow_id_in_both_url_and_payload():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"id": "t1"})
    agent = Agent.generate()

    client.confirm_task_escrow(agent, "e1")

    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/tasks/escrow/e1/confirm"
    assert kwargs["json"]["payload"] == {"escrow_id": "e1"}


def test_claim_task_uses_the_task_id_in_both_url_and_payload():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"id": "t1", "status": "Claimed"})
    agent = Agent.generate()

    client.claim_task(agent, "t1")

    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/tasks/t1/claim"
    assert kwargs["json"]["payload"] == {"task_id": "t1"}


def test_submit_task_url_and_payload():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"verified": True})
    agent = Agent.generate()

    client.submit_task(agent, "t1", "42")

    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/tasks/t1/submit"
    assert kwargs["json"]["payload"] == {"task_id": "t1", "output": "42"}


def test_dispute_flow_urls_and_payloads():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"id": "t1"})
    challenger = Agent.generate()
    operator = Agent.generate()

    client.create_dispute_escrow(challenger, "t1", "wrong answer")
    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/tasks/t1/dispute/escrow"
    assert kwargs["json"]["payload"] == {"task_id": "t1", "reason": "wrong answer"}

    client.confirm_dispute_escrow(challenger, "t1", "e1")
    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/tasks/t1/dispute/confirm"
    assert kwargs["json"]["payload"] == {"task_id": "t1", "escrow_id": "e1"}

    client.resolve_dispute(operator, "t1", "challenger_wins")
    args, kwargs = client.session.post.call_args
    assert args[0] == "http://hub.test/tasks/t1/dispute/resolve"
    assert kwargs["json"]["payload"] == {"task_id": "t1", "outcome": "challenger_wins"}


def test_non_ok_response_raises_hub_error_with_parsed_body():
    client = make_client_with_mock_session()
    client.session.post.return_value = mock_response({"error": "nope"}, status_code=403)
    agent = Agent.generate()

    with pytest.raises(HubError) as excinfo:
        client.claim_task(agent, "t1")

    assert excinfo.value.status_code == 403
    assert excinfo.value.body == {"error": "nope"}


def test_llms_txt_returns_plain_text_not_json():
    client = make_client_with_mock_session()
    resp = MagicMock()
    resp.text = "# itx agent hub"
    resp.raise_for_status = MagicMock()
    client.session.get.return_value = resp

    result = client.llms_txt()

    assert result == "# itx agent hub"
    client.session.get.assert_called_once_with("http://hub.test/llms.txt", timeout=ANY)
