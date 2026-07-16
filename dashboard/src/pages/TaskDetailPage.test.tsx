import { render, screen } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import * as api from "../api";
import TaskDetailPage from "./TaskDetailPage";

vi.mock("../api");

function renderAt(id: string) {
  return render(
    <MemoryRouter initialEntries={[`/tasks/${id}`]}>
      <Routes>
        <Route path="/tasks/:id" element={<TaskDetailPage />} />
      </Routes>
    </MemoryRouter>,
  );
}

describe("TaskDetailPage", () => {
  it("renders a hash_match task's fields", async () => {
    vi.mocked(api.getTask).mockResolvedValue({
      id: "t1",
      description: "solve this",
      bounty: 500,
      status: "Open",
      poster: "02aa",
      claimant: null,
      failed_attempts: 0,
      min_reputation: 0,
      close_reason: null,
      capabilities: [],
      kind: "hash_match",
    });

    renderAt("t1");

    expect(await screen.findByRole("heading", { name: "solve this" })).toBeInTheDocument();
    expect(api.getTask).toHaveBeenCalledWith("t1");
  });

  it("renders consensus-specific fields", async () => {
    vi.mocked(api.getTask).mockResolvedValue({
      id: "t2",
      description: "vote on this",
      bounty: 900,
      status: "Open",
      poster: "02bb",
      claimant: null,
      failed_attempts: 0,
      min_reputation: 0,
      close_reason: null,
      capabilities: [],
      kind: "consensus",
      num_assignees: 3,
      assignees_joined: 1,
      join_deadline: "2026-01-01T00:00:00+00:00",
      submission_deadline: null,
    });

    renderAt("t2");

    expect(await screen.findByText("1 / 3")).toBeInTheDocument();
  });

  it("renders a filed dispute when present", async () => {
    vi.mocked(api.getTask).mockResolvedValue({
      id: "t3",
      description: "open-ended",
      bounty: 700,
      status: "Disputed",
      poster: "02cc",
      claimant: "02dd",
      failed_attempts: 0,
      min_reputation: 0,
      close_reason: null,
      capabilities: [],
      kind: "disputable",
      answer: "42",
      dispute_deadline: "2026-01-01T00:00:00+00:00",
      dispute: {
        challenger: "02ee",
        reason: "wrong answer",
        bond_amount: 700,
        filed_at: "2026-01-01T00:00:00+00:00",
        resolution: null,
      },
    });

    renderAt("t3");

    expect(await screen.findByText("wrong answer")).toBeInTheDocument();
    expect(screen.getByText("02ee")).toBeInTheDocument();
  });

  it("shows an error message when the request fails", async () => {
    vi.mocked(api.getTask).mockRejectedValue(new Error("not found"));

    renderAt("missing");

    expect(await screen.findByRole("alert")).toHaveTextContent("not found");
  });
});
