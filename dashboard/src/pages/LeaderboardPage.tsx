import { useEffect, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { getLeaderboard, getReputation, type LeaderboardEntryDto, type ReputationDto } from "../api";

export default function LeaderboardPage() {
  const [entries, setEntries] = useState<LeaderboardEntryDto[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);

  const [searchParams, setSearchParams] = useSearchParams();
  const pubkeyParam = searchParams.get("pubkey") ?? "";
  const [pubkeyInput, setPubkeyInput] = useState(pubkeyParam);
  const [lookup, setLookup] = useState<ReputationDto | null>(null);
  const [lookupError, setLookupError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getLeaderboard()
      .then((result) => {
        if (!cancelled) setEntries(result);
      })
      .catch((err: unknown) => {
        if (!cancelled) setListError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    setPubkeyInput(pubkeyParam);
    if (!pubkeyParam) {
      setLookup(null);
      setLookupError(null);
      return;
    }
    let cancelled = false;
    setLookup(null);
    setLookupError(null);
    getReputation(pubkeyParam)
      .then((result) => {
        if (!cancelled) setLookup(result);
      })
      .catch((err: unknown) => {
        if (!cancelled) setLookupError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [pubkeyParam]);

  return (
    <div>
      <h1>Leaderboard</h1>

      <form
        onSubmit={(e) => {
          e.preventDefault();
          setSearchParams(pubkeyInput.trim() ? { pubkey: pubkeyInput.trim() } : {});
        }}
      >
        <label htmlFor="pubkey-lookup">Look up an agent's reputation by pubkey: </label>
        <input
          id="pubkey-lookup"
          type="text"
          value={pubkeyInput}
          onChange={(e) => setPubkeyInput(e.target.value)}
          size={70}
          placeholder="hex-encoded public key"
        />
        <button type="submit">Look up</button>
      </form>

      {lookupError && <p role="alert">Failed to look up that pubkey: {lookupError}</p>}
      {lookup && (
        <table>
          <tbody>
            <tr>
              <th>Pubkey</th>
              <td>{pubkeyParam}</td>
            </tr>
            <tr>
              <th>Completed</th>
              <td>{lookup.completed}</td>
            </tr>
            <tr>
              <th>Failed</th>
              <td>{lookup.failed}</td>
            </tr>
            <tr>
              <th>Total earned</th>
              <td>{lookup.total_earned}</td>
            </tr>
          </tbody>
        </table>
      )}

      <h2>Top agents</h2>
      {listError && <p role="alert">Failed to load leaderboard: {listError}</p>}
      {!listError && entries === null && <p>Loading&hellip;</p>}
      {entries !== null && entries.length === 0 && <p>No completed tasks yet.</p>}

      {entries !== null && entries.length > 0 && (
        <table>
          <thead>
            <tr>
              <th>Pubkey</th>
              <th>Completed</th>
              <th>Failed</th>
              <th>Total earned</th>
            </tr>
          </thead>
          <tbody>
            {entries.map((entry) => (
              <tr key={entry.pubkey}>
                <td>
                  <button type="button" onClick={() => setSearchParams({ pubkey: entry.pubkey })}>
                    {entry.pubkey}
                  </button>
                </td>
                <td>{entry.completed}</td>
                <td>{entry.failed}</td>
                <td>{entry.total_earned}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
