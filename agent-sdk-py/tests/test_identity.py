"""Unit tests for `identity.load_or_create_agent` -- the file-based
load-or-generate pattern MCP servers and worked-example scripts use so
an agent's pubkey (and, via the hub, its reputation/balance) survives a
process restart.
"""

from itx_agent_sdk.identity import load_or_create_agent


def test_generates_and_persists_a_new_identity_when_file_is_absent(tmp_path):
    key_file = tmp_path / "agent.key"
    assert not key_file.exists()

    agent = load_or_create_agent(str(key_file))

    assert key_file.exists()
    assert key_file.read_text(encoding="utf-8").strip() == agent.private_key_hex


def test_loads_the_same_identity_on_a_second_call(tmp_path):
    key_file = tmp_path / "agent.key"

    first = load_or_create_agent(str(key_file))
    second = load_or_create_agent(str(key_file))

    assert first.pubkey_hex == second.pubkey_hex
    assert first.private_key_hex == second.private_key_hex


def test_creates_missing_parent_directories(tmp_path):
    key_file = tmp_path / "nested" / "dir" / "agent.key"

    agent = load_or_create_agent(str(key_file))

    assert key_file.exists()
    assert load_or_create_agent(str(key_file)).pubkey_hex == agent.pubkey_hex
