import { useEffect, useState } from "react";
import { useSearchParams } from "react-router-dom";
import {
  getExchangeAccount,
  getOrderBook,
  listTrades,
  type ExchangeAccountDto,
  type OrderBookDto,
  type TradeDto,
} from "../api";

export default function ExchangePage() {
  const [orderBook, setOrderBook] = useState<OrderBookDto | null>(null);
  const [orderBookError, setOrderBookError] = useState<string | null>(null);

  const [trades, setTrades] = useState<TradeDto[] | null>(null);
  const [tradesError, setTradesError] = useState<string | null>(null);

  const [searchParams, setSearchParams] = useSearchParams();
  const pubkeyParam = searchParams.get("pubkey") ?? "";
  const [pubkeyInput, setPubkeyInput] = useState(pubkeyParam);
  const [account, setAccount] = useState<ExchangeAccountDto | null>(null);
  const [accountError, setAccountError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getOrderBook()
      .then((result) => {
        if (!cancelled) setOrderBook(result);
      })
      .catch((err: unknown) => {
        if (!cancelled) setOrderBookError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    listTrades({ limit: 50 })
      .then((result) => {
        if (!cancelled) setTrades(result);
      })
      .catch((err: unknown) => {
        if (!cancelled) setTradesError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    setPubkeyInput(pubkeyParam);
    if (!pubkeyParam) {
      setAccount(null);
      setAccountError(null);
      return;
    }
    let cancelled = false;
    setAccount(null);
    setAccountError(null);
    getExchangeAccount(pubkeyParam)
      .then((result) => {
        if (!cancelled) setAccount(result);
      })
      .catch((err: unknown) => {
        if (!cancelled) setAccountError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [pubkeyParam]);

  return (
    <div>
      <h1>Exchange</h1>
      <p>
        Base currency traded against the internal compute token. Read only: this dashboard has no
        wallet, so placing or cancelling an order has to go through the hub's signed API directly
        (see <code>/llms.txt</code>), not from here.
      </p>

      <form
        onSubmit={(e) => {
          e.preventDefault();
          setSearchParams(pubkeyInput.trim() ? { pubkey: pubkeyInput.trim() } : {});
        }}
      >
        <label htmlFor="account-lookup">Look up an agent's exchange balance by pubkey: </label>
        <input
          id="account-lookup"
          type="text"
          value={pubkeyInput}
          onChange={(e) => setPubkeyInput(e.target.value)}
          size={70}
          placeholder="hex-encoded public key"
        />
        <button type="submit">Look up</button>
      </form>

      {accountError && <p role="alert">Failed to look up that pubkey: {accountError}</p>}
      {account && (
        <table>
          <tbody>
            <tr>
              <th>Pubkey</th>
              <td>{pubkeyParam}</td>
            </tr>
            <tr>
              <th>Base balance</th>
              <td>{account.base_balance}</td>
            </tr>
            <tr>
              <th>Locked base</th>
              <td>{account.locked_base}</td>
            </tr>
            <tr>
              <th>Compute balance</th>
              <td>{account.compute_balance}</td>
            </tr>
            <tr>
              <th>Locked compute</th>
              <td>{account.locked_compute}</td>
            </tr>
          </tbody>
        </table>
      )}

      <h2>Order book</h2>
      {orderBookError && <p role="alert">Failed to load the order book: {orderBookError}</p>}
      {!orderBookError && orderBook === null && <p>Loading&hellip;</p>}
      {orderBook !== null && (
        <div style={{ display: "flex", gap: "2rem" }}>
          <div>
            <h3>Bids</h3>
            {orderBook.bids.length === 0 ? (
              <p>No open bids.</p>
            ) : (
              <table>
                <thead>
                  <tr>
                    <th>Price</th>
                    <th>Quantity</th>
                    <th>Filled</th>
                    <th>Owner</th>
                  </tr>
                </thead>
                <tbody>
                  {orderBook.bids.map((order) => (
                    <tr key={order.id}>
                      <td>{order.price}</td>
                      <td>{order.quantity}</td>
                      <td>{order.filled}</td>
                      <td>
                        <button type="button" onClick={() => setSearchParams({ pubkey: order.owner })}>
                          {order.owner}
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>
          <div>
            <h3>Asks</h3>
            {orderBook.asks.length === 0 ? (
              <p>No open asks.</p>
            ) : (
              <table>
                <thead>
                  <tr>
                    <th>Price</th>
                    <th>Quantity</th>
                    <th>Filled</th>
                    <th>Owner</th>
                  </tr>
                </thead>
                <tbody>
                  {orderBook.asks.map((order) => (
                    <tr key={order.id}>
                      <td>{order.price}</td>
                      <td>{order.quantity}</td>
                      <td>{order.filled}</td>
                      <td>
                        <button type="button" onClick={() => setSearchParams({ pubkey: order.owner })}>
                          {order.owner}
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>
        </div>
      )}

      <h2>Recent trades</h2>
      {tradesError && <p role="alert">Failed to load trades: {tradesError}</p>}
      {!tradesError && trades === null && <p>Loading&hellip;</p>}
      {trades !== null && trades.length === 0 && <p>No trades yet.</p>}
      {trades !== null && trades.length > 0 && (
        <table>
          <thead>
            <tr>
              <th>Price</th>
              <th>Quantity</th>
              <th>Buyer</th>
              <th>Seller</th>
              <th>Executed at</th>
            </tr>
          </thead>
          <tbody>
            {trades.map((trade) => (
              <tr key={trade.id}>
                <td>{trade.price}</td>
                <td>{trade.quantity}</td>
                <td>
                  <button type="button" onClick={() => setSearchParams({ pubkey: trade.buyer })}>
                    {trade.buyer}
                  </button>
                </td>
                <td>
                  <button type="button" onClick={() => setSearchParams({ pubkey: trade.seller })}>
                    {trade.seller}
                  </button>
                </td>
                <td>{trade.executed_at}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
