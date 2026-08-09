import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import * as api from "../api";
import ExchangePage from "./ExchangePage";

vi.mock("../api");

function renderPage() {
  return render(
    <MemoryRouter>
      <ExchangePage />
    </MemoryRouter>,
  );
}

describe("ExchangePage", () => {
  it("renders the order book split into bids and asks", async () => {
    vi.mocked(api.getOrderBook).mockResolvedValue({
      bids: [
        { id: "o1", owner: "02aa", side: "buy", price: 8, quantity: 10, filled: 0, status: "open", created_at: "t" },
      ],
      asks: [
        { id: "o2", owner: "02bb", side: "sell", price: 12, quantity: 5, filled: 0, status: "open", created_at: "t" },
      ],
    });
    vi.mocked(api.listTrades).mockResolvedValue([]);

    renderPage();

    expect(await screen.findByRole("button", { name: "02aa" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "02bb" })).toBeInTheDocument();
    expect(screen.getByText("8")).toBeInTheDocument();
    expect(screen.getByText("12")).toBeInTheDocument();
  });

  it("shows a message when there are no open orders on a side", async () => {
    vi.mocked(api.getOrderBook).mockResolvedValue({ bids: [], asks: [] });
    vi.mocked(api.listTrades).mockResolvedValue([]);

    renderPage();

    expect(await screen.findByText("No open bids.")).toBeInTheDocument();
    expect(screen.getByText("No open asks.")).toBeInTheDocument();
  });

  it("renders recent trades", async () => {
    vi.mocked(api.getOrderBook).mockResolvedValue({ bids: [], asks: [] });
    vi.mocked(api.listTrades).mockResolvedValue([
      {
        id: "t1",
        buy_order_id: "o1",
        sell_order_id: "o2",
        buyer: "02aa",
        seller: "02bb",
        price: 8,
        quantity: 50,
        executed_at: "2026-08-09T00:00:00Z",
        taker_side: "buy",
        taker_fee: 0,
      },
    ]);

    renderPage();

    expect(await screen.findByRole("button", { name: "02aa" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "02bb" })).toBeInTheDocument();
    expect(screen.getByText("50")).toBeInTheDocument();
  });

  it("shows a message when there are no trades yet", async () => {
    vi.mocked(api.getOrderBook).mockResolvedValue({ bids: [], asks: [] });
    vi.mocked(api.listTrades).mockResolvedValue([]);

    renderPage();

    expect(await screen.findByText("No trades yet.")).toBeInTheDocument();
  });

  it("looks up an exchange account by pubkey typed into the form", async () => {
    vi.mocked(api.getOrderBook).mockResolvedValue({ bids: [], asks: [] });
    vi.mocked(api.listTrades).mockResolvedValue([]);
    vi.mocked(api.getExchangeAccount).mockResolvedValue({
      base_balance: 9_000,
      locked_base: 500,
      compute_balance: 20,
      locked_compute: 5,
    });
    const user = userEvent.setup();

    renderPage();

    await user.type(screen.getByLabelText(/look up an agent's exchange balance/i), "02cc");
    await user.click(screen.getByRole("button", { name: /look up/i }));

    expect(await screen.findByText("9000")).toBeInTheDocument();
    expect(api.getExchangeAccount).toHaveBeenCalledWith("02cc");
  });

  it("looks up an exchange account by clicking an order owner", async () => {
    vi.mocked(api.getOrderBook).mockResolvedValue({
      bids: [
        { id: "o1", owner: "02dd", side: "buy", price: 8, quantity: 10, filled: 0, status: "open", created_at: "t" },
      ],
      asks: [],
    });
    vi.mocked(api.listTrades).mockResolvedValue([]);
    vi.mocked(api.getExchangeAccount).mockResolvedValue({
      base_balance: 1_000,
      locked_base: 0,
      compute_balance: 0,
      locked_compute: 0,
    });
    const user = userEvent.setup();

    renderPage();

    await user.click(await screen.findByRole("button", { name: "02dd" }));

    await waitFor(() => expect(api.getExchangeAccount).toHaveBeenCalledWith("02dd"));
  });

  it("shows an error message when the order book request fails", async () => {
    vi.mocked(api.getOrderBook).mockRejectedValue(new Error("network down"));
    vi.mocked(api.listTrades).mockResolvedValue([]);

    renderPage();

    expect(await screen.findByRole("alert")).toHaveTextContent("network down");
  });
});
