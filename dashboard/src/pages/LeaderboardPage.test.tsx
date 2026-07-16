import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import * as api from "../api";
import LeaderboardPage from "./LeaderboardPage";

vi.mock("../api");

describe("LeaderboardPage", () => {
  it("renders leaderboard entries", async () => {
    vi.mocked(api.getLeaderboard).mockResolvedValue([{ pubkey: "02aa", completed: 3, failed: 1, total_earned: 3000 }]);

    render(
      <MemoryRouter>
        <LeaderboardPage />
      </MemoryRouter>,
    );

    expect(await screen.findByText("02aa")).toBeInTheDocument();
    expect(screen.getByText("3000")).toBeInTheDocument();
  });

  it("looks up a pubkey typed into the form", async () => {
    vi.mocked(api.getLeaderboard).mockResolvedValue([]);
    vi.mocked(api.getReputation).mockResolvedValue({ completed: 5, failed: 0, total_earned: 9000 });
    const user = userEvent.setup();

    render(
      <MemoryRouter>
        <LeaderboardPage />
      </MemoryRouter>,
    );

    await user.type(screen.getByLabelText(/look up an agent's reputation/i), "02bb");
    await user.click(screen.getByRole("button", { name: /look up/i }));

    expect(await screen.findByText("9000")).toBeInTheDocument();
    expect(api.getReputation).toHaveBeenCalledWith("02bb");
  });

  it("looks up a pubkey clicked from the leaderboard table", async () => {
    vi.mocked(api.getLeaderboard).mockResolvedValue([{ pubkey: "02cc", completed: 2, failed: 0, total_earned: 4000 }]);
    vi.mocked(api.getReputation).mockResolvedValue({ completed: 2, failed: 0, total_earned: 4000 });
    const user = userEvent.setup();

    render(
      <MemoryRouter>
        <LeaderboardPage />
      </MemoryRouter>,
    );

    await user.click(await screen.findByRole("button", { name: "02cc" }));

    await waitFor(() => expect(api.getReputation).toHaveBeenCalledWith("02cc"));
  });

  it("shows an error message when the leaderboard request fails", async () => {
    vi.mocked(api.getLeaderboard).mockRejectedValue(new Error("network down"));

    render(
      <MemoryRouter>
        <LeaderboardPage />
      </MemoryRouter>,
    );

    expect(await screen.findByRole("alert")).toHaveTextContent("network down");
  });
});
