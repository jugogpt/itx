import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import * as api from "../api";
import TaskListPage from "./TaskListPage";

vi.mock("../api");

const sampleTask: api.TaskDto = {
  id: "11111111-1111-1111-1111-111111111111",
  description: "translate this doc",
  bounty: 1000,
  status: "Open",
  poster: "02aabbcc",
  claimant: null,
  failed_attempts: 0,
  min_reputation: 0,
  close_reason: null,
  capabilities: ["translation"],
  kind: "hash_match",
};

describe("TaskListPage", () => {
  it("renders tasks returned by the hub", async () => {
    vi.mocked(api.listTasks).mockResolvedValue([sampleTask]);

    render(
      <MemoryRouter>
        <TaskListPage />
      </MemoryRouter>,
    );

    expect(await screen.findByText("translate this doc")).toBeInTheDocument();
    expect(screen.getByText("translation")).toBeInTheDocument();
  });

  it("shows an error message when the request fails", async () => {
    vi.mocked(api.listTasks).mockRejectedValue(new Error("network down"));

    render(
      <MemoryRouter>
        <TaskListPage />
      </MemoryRouter>,
    );

    expect(await screen.findByRole("alert")).toHaveTextContent("network down");
  });

  it("shows an empty state when there are no tasks", async () => {
    vi.mocked(api.listTasks).mockResolvedValue([]);

    render(
      <MemoryRouter>
        <TaskListPage />
      </MemoryRouter>,
    );

    expect(await screen.findByText("No open tasks.")).toBeInTheDocument();
  });

  it("re-queries with the capability filter as the user types", async () => {
    vi.mocked(api.listTasks).mockResolvedValue([]);
    const user = userEvent.setup();

    render(
      <MemoryRouter>
        <TaskListPage />
      </MemoryRouter>,
    );

    await waitFor(() => expect(api.listTasks).toHaveBeenCalledWith({ limit: 100, capability: undefined }));

    await user.type(screen.getByLabelText(/filter by capability/i), "python");

    await waitFor(() => expect(api.listTasks).toHaveBeenLastCalledWith({ limit: 100, capability: "python" }));
  });
});
