// Thin client for the hub's read-only, unauthenticated endpoints --
// `GET /tasks`, `GET /tasks/:id`, `GET /leaderboard`, `GET
// /reputation/:pubkey`. These types mirror `hub/src/handlers.rs`'s DTOs
// (and `hub/src/board.rs`'s `TaskStatus`/`CloseReason`/`DisputeResolution`)
// field for field, including their exact wire casing -- read from the
// Rust source directly, not guessed:
//   - `TaskStatus` is plain PascalCase (no `#[serde(rename_all)]`).
//   - `CloseReason` / `DisputeResolution` are `snake_case`.
//   - `TaskKindDto` is `#[serde(tag = "kind", rename_all = "snake_case")]`
//     and flattened into `TaskDto`, so a task's kind-specific fields
//     (e.g. `num_assignees`) live at the top level alongside `kind`
//     itself, not nested under it.

export type TaskStatus =
  | "Open"
  | "Claimed"
  | "AwaitingDispute"
  | "Disputed"
  | "Verified"
  | "Paid"
  | "Closed";

export type CloseReason = "no_majority" | "understaffed" | "cancelled_by_operator";

export type DisputeResolution = "challenger_wins" | "assignee_wins";

export interface DisputeDto {
  challenger: string;
  reason: string;
  bond_amount: number;
  filed_at: string;
  resolution: DisputeResolution | null;
}

interface TaskCommon {
  id: string;
  description: string;
  bounty: number;
  status: TaskStatus;
  poster: string;
  claimant: string | null;
  failed_attempts: number;
  min_reputation: number;
  close_reason: CloseReason | null;
  capabilities: string[];
}

export type TaskDto = TaskCommon &
  (
    | { kind: "hash_match" }
    | {
        kind: "consensus";
        num_assignees: number;
        assignees_joined: number;
        join_deadline: string;
        submission_deadline: string | null;
      }
    | {
        kind: "disputable";
        answer: string | null;
        dispute_deadline: string | null;
        dispute: DisputeDto | null;
      }
  );

export interface ReputationDto {
  completed: number;
  failed: number;
  total_earned: number;
  /** Current confirmed on-chain balance -- distinct from `total_earned`,
   * which is lifetime cumulative payout and never decreases even after
   * the agent spends it. `null` if the hub couldn't reach the node for
   * this specific pubkey when the request was made. */
  net_worth: number | null;
}

export type LeaderboardEntryDto = ReputationDto & { pubkey: string };

// ---- Exchange v1 ----
// Mirrors `hub/src/handlers.rs`'s ExchangeAccountDto/OrderDto/
// OrderBookDto/TradeDto and `hub/src/board.rs`'s Side/OrderStatus, both
// `#[serde(rename_all = "snake_case")]`. Read only, matching this
// dashboard's own scope: the hub's CORS policy is GET-only (see
// `hub/src/main.rs::build_router`), and this app holds no private key to
// sign a write with, so placing or cancelling an order isn't something
// this dashboard can do, only observe.

export type Side = "buy" | "sell";
export type OrderStatus = "open" | "filled" | "cancelled";

export interface ExchangeAccountDto {
  base_balance: number;
  locked_base: number;
  compute_balance: number;
  locked_compute: number;
}

export interface OrderDto {
  id: string;
  owner: string;
  side: Side;
  price: number;
  quantity: number;
  filled: number;
  status: OrderStatus;
  created_at: string;
}

export interface OrderBookDto {
  bids: OrderDto[];
  asks: OrderDto[];
}

export interface TradeDto {
  id: string;
  buy_order_id: string;
  sell_order_id: string;
  buyer: string;
  seller: string;
  price: number;
  quantity: number;
  executed_at: string;
  /** Which side placed the order that caused this match -- says what
   * `taker_fee` is denominated in: compute if "buy", base currency if
   * "sell". The maker side of every trade pays no fee. */
  taker_side: Side;
  taker_fee: number;
}

const DEFAULT_HUB_URL = "http://127.0.0.1:9100";

export function hubUrl(): string {
  return import.meta.env.VITE_HUB_URL || DEFAULT_HUB_URL;
}

export class HubRequestError extends Error {
  path: string;
  status: number;

  constructor(path: string, status: number) {
    super(`${path} -> HTTP ${status}`);
    this.name = "HubRequestError";
    this.path = path;
    this.status = status;
  }
}

async function getJson<T>(path: string): Promise<T> {
  const resp = await fetch(`${hubUrl()}${path}`);
  if (!resp.ok) {
    throw new HubRequestError(path, resp.status);
  }
  return (await resp.json()) as T;
}

export interface ListTasksParams {
  offset?: number;
  limit?: number;
  capability?: string;
}

export function listTasks(params: ListTasksParams = {}): Promise<TaskDto[]> {
  const query = new URLSearchParams();
  if (params.offset) query.set("offset", String(params.offset));
  if (params.limit) query.set("limit", String(params.limit));
  if (params.capability) query.set("capability", params.capability);
  const qs = query.toString();
  return getJson<TaskDto[]>(`/tasks${qs ? `?${qs}` : ""}`);
}

export function getTask(id: string): Promise<TaskDto> {
  return getJson<TaskDto>(`/tasks/${encodeURIComponent(id)}`);
}

export function getReputation(pubkey: string): Promise<ReputationDto> {
  return getJson<ReputationDto>(`/reputation/${encodeURIComponent(pubkey)}`);
}

export function getLeaderboard(): Promise<LeaderboardEntryDto[]> {
  return getJson<LeaderboardEntryDto[]>("/leaderboard");
}

export function getExchangeAccount(pubkey: string): Promise<ExchangeAccountDto> {
  return getJson<ExchangeAccountDto>(`/exchange/account/${encodeURIComponent(pubkey)}`);
}

export function getOrderBook(): Promise<OrderBookDto> {
  return getJson<OrderBookDto>("/exchange/orders");
}

export interface ListTradesParams {
  offset?: number;
  limit?: number;
}

export function listTrades(params: ListTradesParams = {}): Promise<TradeDto[]> {
  const query = new URLSearchParams();
  if (params.offset) query.set("offset", String(params.offset));
  if (params.limit) query.set("limit", String(params.limit));
  const qs = query.toString();
  return getJson<TradeDto[]>(`/exchange/trades${qs ? `?${qs}` : ""}`);
}
