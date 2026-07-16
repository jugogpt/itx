import { useEffect, useState } from "react";
import { Link, useParams } from "react-router-dom";
import { getTask, type TaskDto } from "../api";

export default function TaskDetailPage() {
  const { id } = useParams<{ id: string }>();
  const [task, setTask] = useState<TaskDto | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!id) return;
    let cancelled = false;
    setTask(null);
    setError(null);
    getTask(id)
      .then((result) => {
        if (!cancelled) setTask(result);
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [id]);

  return (
    <div>
      <p>
        <Link to="/">&larr; back to tasks</Link>
      </p>

      {error && <p role="alert">Failed to load task: {error}</p>}
      {!error && !task && <p>Loading&hellip;</p>}

      {task && (
        <>
          <h1>{task.description}</h1>
          <table>
            <tbody>
              <tr>
                <th>ID</th>
                <td>{task.id}</td>
              </tr>
              <tr>
                <th>Kind</th>
                <td>{task.kind}</td>
              </tr>
              <tr>
                <th>Status</th>
                <td>{task.status}</td>
              </tr>
              <tr>
                <th>Bounty</th>
                <td>{task.bounty}</td>
              </tr>
              <tr>
                <th>Poster</th>
                <td>
                  <Link to={`/leaderboard?pubkey=${encodeURIComponent(task.poster)}`}>{task.poster}</Link>
                </td>
              </tr>
              <tr>
                <th>Claimant</th>
                <td>
                  {task.claimant ? (
                    <Link to={`/leaderboard?pubkey=${encodeURIComponent(task.claimant)}`}>{task.claimant}</Link>
                  ) : (
                    "(none yet)"
                  )}
                </td>
              </tr>
              <tr>
                <th>Failed attempts</th>
                <td>{task.failed_attempts}</td>
              </tr>
              <tr>
                <th>Minimum reputation</th>
                <td>{task.min_reputation}</td>
              </tr>
              <tr>
                <th>Capabilities</th>
                <td>{task.capabilities.length > 0 ? task.capabilities.join(", ") : "(none)"}</td>
              </tr>
              {task.close_reason && (
                <tr>
                  <th>Close reason</th>
                  <td>{task.close_reason}</td>
                </tr>
              )}

              {task.kind === "consensus" && (
                <>
                  <tr>
                    <th>Assignees</th>
                    <td>
                      {task.assignees_joined} / {task.num_assignees}
                    </td>
                  </tr>
                  <tr>
                    <th>Join deadline</th>
                    <td>{task.join_deadline}</td>
                  </tr>
                  <tr>
                    <th>Submission deadline</th>
                    <td>{task.submission_deadline ?? "(not started)"}</td>
                  </tr>
                </>
              )}

              {task.kind === "disputable" && (
                <>
                  <tr>
                    <th>Submitted answer</th>
                    <td>{task.answer ?? "(not submitted yet)"}</td>
                  </tr>
                  <tr>
                    <th>Dispute deadline</th>
                    <td>{task.dispute_deadline ?? "(not started)"}</td>
                  </tr>
                  {task.dispute && (
                    <>
                      <tr>
                        <th>Disputed by</th>
                        <td>{task.dispute.challenger}</td>
                      </tr>
                      <tr>
                        <th>Dispute reason</th>
                        <td>{task.dispute.reason}</td>
                      </tr>
                      <tr>
                        <th>Bond amount</th>
                        <td>{task.dispute.bond_amount}</td>
                      </tr>
                      <tr>
                        <th>Filed at</th>
                        <td>{task.dispute.filed_at}</td>
                      </tr>
                      <tr>
                        <th>Resolution</th>
                        <td>{task.dispute.resolution ?? "(pending)"}</td>
                      </tr>
                    </>
                  )}
                </>
              )}
            </tbody>
          </table>
        </>
      )}
    </div>
  );
}
