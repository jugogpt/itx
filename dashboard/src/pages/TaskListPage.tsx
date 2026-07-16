import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { listTasks, type TaskDto } from "../api";

export default function TaskListPage() {
  const [capability, setCapability] = useState("");
  const [tasks, setTasks] = useState<TaskDto[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setTasks(null);
    setError(null);
    listTasks({ limit: 100, capability: capability.trim() || undefined })
      .then((result) => {
        if (!cancelled) setTasks(result);
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [capability]);

  return (
    <div>
      <h1>Tasks</h1>

      <p>
        <label htmlFor="capability-filter">Filter by capability: </label>
        <input
          id="capability-filter"
          type="text"
          value={capability}
          onChange={(e) => setCapability(e.target.value)}
          placeholder="e.g. python"
        />
      </p>

      {error && <p role="alert">Failed to load tasks: {error}</p>}
      {!error && tasks === null && <p>Loading&hellip;</p>}
      {tasks !== null && tasks.length === 0 && <p>No open tasks.</p>}

      {tasks !== null && tasks.length > 0 && (
        <table>
          <thead>
            <tr>
              <th>Description</th>
              <th>Kind</th>
              <th>Status</th>
              <th>Bounty</th>
              <th>Min. reputation</th>
              <th>Capabilities</th>
            </tr>
          </thead>
          <tbody>
            {tasks.map((task) => (
              <tr key={task.id}>
                <td>
                  <Link to={`/tasks/${task.id}`}>{task.description}</Link>
                </td>
                <td>{task.kind}</td>
                <td>{task.status}</td>
                <td>{task.bounty}</td>
                <td>{task.min_reputation}</td>
                <td>{task.capabilities.join(", ")}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
