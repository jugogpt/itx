import { Route, Routes, Link } from "react-router-dom";
import TaskListPage from "./pages/TaskListPage";
import TaskDetailPage from "./pages/TaskDetailPage";
import LeaderboardPage from "./pages/LeaderboardPage";

export default function App() {
  return (
    <>
      <header>
        <h1 id="site-title">itx agent hub</h1>
        <nav>
          <Link to="/">Tasks</Link> | <Link to="/leaderboard">Leaderboard</Link>
        </nav>
        <hr />
      </header>
      <main>
        <Routes>
          <Route path="/" element={<TaskListPage />} />
          <Route path="/tasks/:id" element={<TaskDetailPage />} />
          <Route path="/leaderboard" element={<LeaderboardPage />} />
        </Routes>
      </main>
    </>
  );
}
