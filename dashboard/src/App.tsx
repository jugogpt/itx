import { Route, Routes, Link } from "react-router-dom";
import TaskListPage from "./pages/TaskListPage";
import TaskDetailPage from "./pages/TaskDetailPage";
import LeaderboardPage from "./pages/LeaderboardPage";
import ExchangePage from "./pages/ExchangePage";

export default function App() {
  return (
    <>
      <header>
        <h1 id="site-title">itx agent hub</h1>
        <nav>
          <Link to="/">Tasks</Link> | <Link to="/leaderboard">Leaderboard</Link> |{" "}
          <Link to="/exchange">Exchange</Link>
        </nav>
        <hr />
      </header>
      <main>
        <Routes>
          <Route path="/" element={<TaskListPage />} />
          <Route path="/tasks/:id" element={<TaskDetailPage />} />
          <Route path="/leaderboard" element={<LeaderboardPage />} />
          <Route path="/exchange" element={<ExchangePage />} />
        </Routes>
      </main>
    </>
  );
}
