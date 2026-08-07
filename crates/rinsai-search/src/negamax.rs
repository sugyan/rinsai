//! Iterative-deepening negamax with alpha-beta pruning.
//!
//! E0 step 2's search, and deliberately the plainest one that plays: no
//! transposition table and no quiescence (step 3), no move ordering at all
//! (step 3's transposition move, then E1's), no time *allocation* (step 5).
//! It will hang pieces — evaluating material at a fixed depth with no
//! quiescence is the horizon effect by construction, which DESIGN.md names as
//! the reason the two belong together. That is expected for one step; the
//! answer is step 3, not a patch here.
//!
//! The two structural decisions worth knowing before reading:
//!
//! * The recursion carries a [`Window`] rather than a loose `(alpha, beta)`
//!   pair, so the child call is `-self.negamax(board, -window, …)` and the
//!   classic `-search(-beta, -alpha)` transposition cannot be mistyped.
//! * The board is a **parameter**, never a field. `self.negamax(…)` while
//!   `&mut self.board` is live does not compile, and the tempting repairs
//!   (`RefCell`, `mem::take`) are all worse than passing it.

use core::ops::{ControlFlow, Neg};
use std::time::Instant;

use shogi_core::Move;
use shunsai::Position;

use crate::eval;
use crate::info::SearchInfo;
use crate::moves::{MAX_LEGAL_MOVES, MoveBuf};
use crate::score::{Depth, MAX_PLY, Score};
use crate::search::{BestMove, InfoSink, Limits, SearchJob, SearchSignals, Searcher};

/// How deep a `go` that named no budget at all searches.
///
/// **It has to be even.** The root is the engine's own move, so an odd depth
/// ends the line on a capture of ours that the opponent never answers — the
/// search then values grabbing material it cannot keep. An even depth ends
/// after the reply, which is the pessimistic side to be on while there is no
/// quiescence search to settle the exchange properly.
///
/// Four rather than six because there is no interior move ordering yet, so the
/// tree is close to a full minimax and each further pair of plies costs an order
/// of magnitude. Six would be a long think for a move nobody asked to have
/// thought about. The node counts and timings behind that are in PROGRESS.md,
/// not here — a measurement copied into a doc comment is a measurement that
/// goes stale silently.
const DEFAULT_DEPTH: Depth = 4;

/// How often the search asks whether it is out of budget.
///
/// The conventional starting point, and untuned. It is far inside any USI
/// deadline at this step's node rate, because most nodes are depth-zero leaves
/// that only evaluate (the rate itself is in PROGRESS.md). Expect the interval
/// to buy less latency as a node gets more expensive — quiescence at step 3,
/// ordering work at E1 — and expect step 5's time-management SPRT to be where
/// the number is chosen rather than inherited.
const POLL_INTERVAL_NODES: u64 = 1024;

/// The deepest iteration any search will start. One below [`MAX_PLY`] so that
/// a line reaching the last iteration's nominal depth still has a ply to sit
/// at.
const MAX_DEPTH: Depth = MAX_PLY as Depth - 1;

/// An alpha-beta window, negated as a unit.
#[derive(Clone, Copy, Debug)]
struct Window {
    alpha: Score,
    beta: Score,
}

impl Window {
    /// The full window, and **the only one [`NegamaxSearcher::negamax_root`]
    /// accepts today** — it asserts as much, and the reason is in that method's
    /// doc. E1's aspiration search is what will want a narrower one, and
    /// widening the root's contract is part of that step rather than something
    /// already paid for here.
    fn open() -> Self {
        Self {
            alpha: -Score::INFINITE,
            beta: Score::INFINITE,
        }
    }
}

impl Neg for Window {
    type Output = Self;

    /// Swap and negate, which is what makes the child's window the parent's
    /// seen from the other side.
    fn neg(self) -> Self {
        Self {
            alpha: -self.beta,
            beta: -self.alpha,
        }
    }
}

/// What this search is allowed to spend.
///
/// The boundary it draws is **a budget it was told** versus **a budget it would
/// have to decide**. `movetime` and `byoyomi` state how long this move gets, so
/// they are honoured here; `btime` / `wtime` / `binc` / `winc` state what is
/// left on the clock, and turning that into a per-move allowance — with the
/// fail-low extension, the early cut-off on an obvious move and the network
/// delay margin that go with it — is E0 step 5's whole subject and is not
/// approximated here.
struct Budget<'a> {
    signals: &'a SearchSignals,
    deadline: Option<Instant>,
    node_limit: Option<u64>,
    max_depth: Depth,
}

impl<'a> Budget<'a> {
    fn new(limits: &Limits, signals: &'a SearchSignals, started: Instant) -> Self {
        // `movetime` before `byoyomi`: it is the more specific of the two, and
        // a GUI that sends both means the explicit one. `byoyomi 0` is "this
        // time control has no byoyomi" — read as a duration it would give every
        // move a zero-millisecond budget.
        let stated = limits
            .movetime
            .or_else(|| limits.byoyomi.filter(|d| !d.is_zero()));
        // `infinite` means "until told to stop", which outranks any clock.
        let deadline = (!limits.infinite)
            .then_some(stated)
            .flatten()
            .map(|until| started + until);

        // `DEFAULT_DEPTH` is the answer to "no budget of any kind was named",
        // and **only** to that. Applying it as a ceiling over a stated budget
        // as well is the bug this shape exists to prevent: `go movetime 1000`
        // would then think for four plies and hand the other 975 ms back.
        let max_depth = match limits.depth {
            Some(depth) => Depth::try_from(depth)
                .unwrap_or(MAX_DEPTH)
                .clamp(1, MAX_DEPTH),
            None if limits.infinite || deadline.is_some() || limits.nodes.is_some() => MAX_DEPTH,
            None => DEFAULT_DEPTH,
        };

        Self {
            signals,
            deadline,
            node_limit: limits.nodes,
            max_depth,
        }
    }

    fn expired(&self, nodes: u64) -> bool {
        self.signals.stopped()
            || self.node_limit.is_some_and(|limit| nodes >= limit)
            || self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
    }
}

/// The searcher.
///
/// Everything that has to survive between searches lives here, because the
/// driver's worker thread owns one for its whole life. At step 2 that is only
/// the allocations; step 3's transposition table and E1's history tables join
/// them.
#[derive(Debug)]
pub struct NegamaxSearcher {
    /// The root's moves, kept out of [`Self::buf`] on purpose: they persist
    /// across iterative-deepening iterations and get reordered, which is not
    /// what a ply-threaded buffer is for. E1's per-root-move bookkeeping —
    /// subtree node counts, MultiPV — belongs here too.
    root_moves: Vec<Move>,
    buf: MoveBuf,
    /// The principal variation from each ply downwards.
    ///
    /// `MAX_PLY + 1` lines, because a node at ply `MAX_PLY - 1` reads the line
    /// below it. A triangular array was the alternative and does not fit:
    /// [`Move`] has no `Default`, so `[[Move; N]; N]` needs `MaybeUninit` and
    /// the workspace denies `unsafe_code`, while `[[Option<Move>; N]; N]` costs
    /// a row clear on every node unless it also carries a per-ply length — at
    /// which point it is a `Vec` with extra steps.
    pv: Vec<Vec<Move>>,
    nodes: u64,
    /// Sticky. Once the budget is spent every frame returns at once without
    /// polling again, so an abandoned subtree cannot spend time on its way out.
    stopped: bool,
}

impl NegamaxSearcher {
    #[must_use]
    pub fn new() -> Self {
        Self {
            root_moves: Vec::with_capacity(MAX_LEGAL_MOVES),
            buf: MoveBuf::new(),
            pv: (0..=MAX_PLY).map(|_| Vec::with_capacity(MAX_PLY)).collect(),
            nodes: 0,
            stopped: false,
        }
    }

    /// One root, all of its moves, at one depth.
    ///
    /// Returns `None` when the iteration produced nothing usable — every root
    /// move was abandoned before it finished — in which case the previous
    /// iteration's [`Self::pv`] and best move stand. `Some` therefore carries
    /// the promise that `pv[0]` is fresh and non-empty, which is what lets the
    /// caller read `pv[0][0]` without checking.
    ///
    /// ⚠️ **`window` must be open, and the assertion below is load-bearing
    /// rather than defensive.** What the code actually returns `None` on is "no
    /// root move raised alpha", and that is only the same thing as "no root move
    /// finished" while alpha starts at `-INFINITE`. Hand this a narrow window
    /// and an ordinary **fail-low** — the case aspiration search exists to
    /// re-search wider — comes back as `None`, whereupon [`Searcher::search`]
    /// breaks out of the deepening loop instead of re-searching. That failure
    /// has no wrong answer and no panic in it: the engine simply stops
    /// deepening, and every test here passes because every test here searches an
    /// open window. Note too that this ignores `window.beta` outright; a root
    /// with a real beta needs a cutoff. E1 owns both, and owes this method a
    /// return type that tells fail-low apart from abandoned.
    fn negamax_root(
        &mut self,
        board: &mut Position,
        mut window: Window,
        depth: Depth,
        budget: &Budget<'_>,
    ) -> Option<Score> {
        debug_assert_eq!(
            window.alpha,
            -Score::INFINITE,
            "negamax_root cannot tell a fail-low from an abandoned iteration"
        );
        self.nodes += 1;

        let mut best = -Score::INFINITE;
        let mut improved = false;
        for i in 0..self.root_moves.len() {
            let mv = self.root_moves[i];
            board.do_move(mv);
            let score = -self.negamax(board, -window, depth - 1, 1, budget);
            board.undo_move(mv);

            // An abandoned subtree returns a placeholder, not a score. Discard
            // it here, before it can beat anything. A move that *did* finish is
            // better informed than the last iteration's answer, so a partial
            // iteration still improves on its predecessor.
            if self.stopped {
                break;
            }
            if score > best {
                best = score;
                if score > window.alpha {
                    window.alpha = score;
                    self.update_pv(0, mv);
                    improved = true;
                }
            }
        }

        improved.then_some(best)
    }

    /// An interior node.
    ///
    /// Not called `search`: [`Searcher::search`] is the trait method on this
    /// same type, and two methods of one name resolving by arity is a puzzle
    /// nobody should have to solve while reading a search.
    fn negamax(
        &mut self,
        board: &mut Position,
        mut window: Window,
        depth: Depth,
        ply: usize,
        budget: &Budget<'_>,
    ) -> Score {
        // First statement, above every early return: the frozen convention is
        // one node per entry, including the root and including a node that
        // turns straight round. Step 3's `bench` freezes these counts.
        self.nodes += 1;
        self.pv[ply].clear();

        if self.stopped {
            return Score::ZERO;
        }
        if self.nodes.is_multiple_of(POLL_INTERVAL_NODES) && budget.expired(self.nodes) {
            self.stopped = true;
            return Score::ZERO;
        }

        // Step 3 replaces this with a call into quiescence search. Note what it
        // costs until then: a node that is *checkmated* at depth zero reports
        // its material balance, because finding that out means generating
        // moves and that is the leaf cost this branch exists to avoid.
        if depth <= 0 || ply >= MAX_PLY {
            return eval::evaluate(board);
        }

        let base = self.buf.generate(board);
        if self.buf.len() == base {
            // Shogi has no stalemate, so no legal move is mate — no `in_check`
            // call needed, and none made. The distance is measured from the
            // search root rather than from the remaining depth, so a mate score
            // means the same thing in every iteration.
            return Score::mated_in(ply);
        }

        let mut best = -Score::INFINITE;
        for i in base..self.buf.len() {
            let mv = self.buf.get(i);
            board.do_move(mv);
            let score = -self.negamax(board, -window, depth - 1, ply + 1, budget);
            // Before every `break` below, without exception: the board has to
            // be whole again for the caller.
            board.undo_move(mv);

            if self.stopped {
                break;
            }
            if score > best {
                best = score;
                if score > window.alpha {
                    window.alpha = score;
                    self.update_pv(ply, mv);
                    if window.alpha >= window.beta {
                        break;
                    }
                }
            }
        }
        self.buf.truncate(base);
        best
    }

    /// Records `mv` as the best move at `ply`, followed by the line below it.
    ///
    /// `split_at_mut` is the answer to needing `pv[ply]` and `pv[ply + 1]` at
    /// once. The copy runs only when alpha is raised, not per node.
    fn update_pv(&mut self, ply: usize, mv: Move) {
        let (head, tail) = self.pv.split_at_mut(ply + 1);
        let line = &mut head[ply];
        line.clear();
        line.push(mv);
        line.extend_from_slice(&tail[0]);
    }

    /// Holds an answer back for as long as `go infinite` says to.
    ///
    /// `infinite` is about when a search stops *searching*, which is the
    /// searcher's own business — including when it has nothing to search, which
    /// is why `resign` comes through here too. `ponder` is about when an answer
    /// may be *sent*, which is a protocol rule the driver holds on every
    /// searcher's behalf (see `search::worker`).
    fn finish(&self, job: &SearchJob, best: BestMove) -> BestMove {
        while job.limits.infinite && !job.signals.stopped() && !job.signals.ponder_hit() {
            job.signals.wait_until_signalled();
        }
        best
    }
}

impl Default for NegamaxSearcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Searcher for NegamaxSearcher {
    fn search(&mut self, job: &SearchJob, out: &dyn InfoSink) -> BestMove {
        let started = Instant::now();
        let budget = Budget::new(&job.limits, &job.signals, started);
        let mut board = job.game.search_board();

        self.nodes = 0;
        self.stopped = false;
        // Not redundant with the per-ply truncation: a panic caught by
        // `search::worker` unwinds past every one of those, and the worker
        // keeps this searcher for the next `go`.
        self.buf.clear();
        for line in &mut self.pv {
            line.clear();
        }

        self.root_moves.clear();
        let root_moves = &mut self.root_moves;
        let _ = board.generate_moves(|set| {
            root_moves.extend(set);
            ControlFlow::Continue(())
        });
        if self.root_moves.is_empty() {
            return self.finish(job, BestMove::Resign);
        }

        // Seeded so that every path out of the loop below has a legal move to
        // report, including "stopped before the first iteration finished".
        // Which move this is, is shunsai's generation order — unspecified, and
        // nothing may depend on it.
        let mut best = self.root_moves[0];

        for depth in 1..=budget.max_depth {
            let Some(score) = self.negamax_root(&mut board, Window::open(), depth, &budget) else {
                break;
            };
            // Taking the answer from a *partial* iteration is deliberate and is
            // safe for exactly one reason: the root list was re-seeded below
            // with the last iteration's move, so every move this iteration did
            // finish was compared against that one at this depth. The `score`
            // beside it is weaker than the move — over a prefix of the root list
            // it is a lower bound, not the iteration's value — and the `info`
            // line does not say so, because USI has no way to. A GUI reading a
            // score that dipped at the last depth of a stopped search is seeing
            // that, not a blunder.
            best = self.pv[0][0];
            out.info(
                &SearchInfo {
                    depth,
                    score,
                    nodes: self.nodes,
                    elapsed: started.elapsed(),
                    pv: &self.pv[0],
                }
                .to_string(),
            );

            // The next iteration starts from this one's answer. Without it,
            // iterative deepening is only repeated work: an iteration cut short
            // reports the best of whatever prefix of the root list it happened
            // to reach, which can be a **worse** move than the last completed
            // iteration chose. Measured before this line existed — a search cut
            // off partway through depth 6 replaced the completed depth-5 answer
            // with one scoring 1 590 cp lower.
            //
            // This is root-only, and it is not the move ordering E1 is about:
            // MVV-LVA, killers and history all order *interior* nodes and each
            // wants its own SPRT. This is the loop reusing its own last result,
            // which is what the loop is for.
            if let Some(index) = self.root_moves.iter().position(|mv| *mv == best) {
                self.root_moves.swap(0, index);
            }

            // A proven mate does not get better with depth: the maximising
            // search already prefers the shortest win and the longest defence.
            if score.is_mate() || self.stopped || budget.expired(self.nodes) {
                break;
            }
        }

        self.finish(
            job,
            BestMove::Play {
                mv: best,
                ponder: None,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;
    use std::time::Duration;

    use shogi_core::ToUsi;

    use super::*;
    use crate::game::Game;
    use crate::moves::is_legal;
    use crate::search::{SearchSignals, SilentSink};

    /// An [`InfoSink`] a test can read back.
    #[derive(Default)]
    struct Lines(Mutex<Vec<String>>);

    impl InfoSink for Lines {
        fn info(&self, line: &str) {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(line.to_owned());
        }
    }

    impl Lines {
        fn take(&self) -> Vec<String> {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }
    }

    fn game(args: &str) -> Game {
        Game::from_usi_position(args).expect("the fixture parses")
    }

    fn job(game: Game, limits: Limits) -> SearchJob {
        SearchJob {
            id: 0,
            game,
            limits,
            signals: Arc::new(SearchSignals::new()),
        }
    }

    fn depth(depth: u32) -> Limits {
        Limits {
            depth: Some(depth),
            ..Limits::default()
        }
    }

    fn run(args: &str, limits: Limits) -> (BestMove, Vec<String>) {
        let sink = Lines::default();
        let best = NegamaxSearcher::new().search(&job(game(args), limits), &sink);
        (best, sink.take())
    }

    /// The value of `name` in an `info` line, as a number.
    fn field(line: &str, name: &str) -> i64 {
        let mut tokens = line.split_whitespace();
        while let Some(token) = tokens.next() {
            if token == name {
                return tokens
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| panic!("`{name}` has no number in {line:?}"));
            }
        }
        panic!("no `{name}` in {line:?}");
    }

    /// The moves after ` pv `, as USI tokens.
    fn pv_of(line: &str) -> Vec<&str> {
        line.split(" pv ")
            .nth(1)
            .map(|tail| tail.split_whitespace().collect())
            .unwrap_or_default()
    }

    #[test]
    fn answers_with_a_legal_move() {
        let fixture = game("startpos");
        let (best, _) = run("startpos", depth(2));
        match best {
            BestMove::Play { mv, ponder } => {
                assert!(is_legal(fixture.position(), mv));
                assert_eq!(ponder, None);
            }
            BestMove::Resign => panic!("the initial position has 30 legal moves"),
        }
    }

    /// A lone king with nothing to move and no hand has no legal move at all —
    /// no mate geometry needed to construct it.
    #[test]
    fn resigns_when_there_is_no_legal_move() {
        let (best, lines) = run("sfen 4k4/9/9/9/9/9/9/9/9 b - 1", depth(4));
        assert_eq!(best, BestMove::Resign);
        assert!(
            lines.is_empty(),
            "reported progress on a search it never ran"
        );
    }

    /// The other way to have no move: actually checkmated. Black golds on 5b
    /// and 5c, White's king on 5a — 5b is protected, and every flight square is
    /// covered by the gold on 5b.
    #[test]
    fn a_checkmated_root_resigns() {
        let fixture = game("sfen 4k4/4G4/4G4/9/9/9/9/9/4K4 w - 1");
        assert!(fixture.in_check(), "the fixture is not even check");
        assert!(
            fixture.position().legal_moves().is_empty(),
            "the fixture is not mate"
        );
        let (best, _) = run("sfen 4k4/4G4/4G4/9/9/9/9/9/4K4 w - 1", depth(4));
        assert_eq!(best, BestMove::Resign);
    }

    #[test]
    fn an_ordinary_go_answers_immediately() {
        assert!(matches!(run("startpos", depth(2)).0, BestMove::Play { .. }));
    }

    /// A searcher does **not** hold a ponder answer back — the driver does.
    #[test]
    fn go_ponder_is_not_the_searchers_business() {
        let limits = Limits {
            ponder: true,
            depth: Some(2),
            ..Limits::default()
        };
        assert!(matches!(run("startpos", limits).0, BestMove::Play { .. }));
    }

    /// `go infinite` must not answer until it is told to, **even when there is
    /// nothing at all to think about**.
    ///
    /// The fixture is the one with no legal move on purpose. Run this on the
    /// initial position instead and the "no answer within 200 ms" assertion
    /// passes because the search is still working — it would stay green with
    /// the wait loop deleted, which is the whole thing it is here to catch.
    /// Sabotage: delete the loop in `finish` and `recv_timeout` below succeeds
    /// where it must time out.
    #[test]
    fn go_infinite_waits_for_stop_even_with_nothing_to_search() {
        let limits = Limits {
            infinite: true,
            ..Limits::default()
        };
        let job = Arc::new(job(game("sfen 4k4/9/9/9/9/9/9/9/9 b - 1"), limits));
        let signals = Arc::clone(&job.signals);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(NegamaxSearcher::new().search(&job, &SilentSink));
        });

        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "answered `go infinite` without being told to"
        );
        signals.stop();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(10))
                .expect("answers once stopped"),
            BestMove::Resign
        );
    }

    /// …and a search that *is* working has to notice `stop` too. This is the
    /// half that the wait loop cannot cover: the poll on the hot path.
    #[test]
    fn an_infinite_search_stops_when_told() {
        let limits = Limits {
            infinite: true,
            ..Limits::default()
        };
        let job = Arc::new(job(game("startpos"), limits));
        let signals = Arc::clone(&job.signals);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(NegamaxSearcher::new().search(&job, &SilentSink));
        });

        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "an infinite search answered on its own"
        );
        signals.stop();
        assert!(
            matches!(
                rx.recv_timeout(Duration::from_secs(10)),
                Ok(BestMove::Play { .. })
            ),
            "an infinite search did not return promptly after `stop`"
        );
    }

    /// The frozen node-counting convention, written as an equation: one node
    /// for the root plus one for each move it tries.
    ///
    /// Sabotage: move `self.nodes += 1` below the `depth <= 0` return in
    /// `negamax` and this drops to 1.
    #[test]
    fn the_root_and_every_leaf_count_as_one_node() {
        for args in [
            "startpos",
            "sfen l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1",
        ] {
            let expected = 1 + game(args).position().legal_moves().len() as i64;
            let (_, lines) = run(args, depth(1));
            assert_eq!(lines.len(), 1, "{args}");
            assert_eq!(field(&lines[0], "nodes"), expected, "{args}");
        }
    }

    /// 頭金: a gold dropped on 5b, protected by the gold on 5c, with every
    /// flight square from 5a covered.
    ///
    /// Note the depth. A mate one ply away is invisible at depth 1, because a
    /// depth-zero node reports material without generating moves — the cost
    /// named in `negamax`, and step 3's quiescence search is what removes it.
    ///
    /// Sabotage, both verified: score a mated node `Score::mated_in(0)` instead
    /// of `mated_in(ply)` and the announced distance is wrong; drop the
    /// `pv[ply].clear()` at node entry and a stale line from an earlier subtree
    /// hangs off the mate. The second is the surprising one — the leaf plies
    /// are the only ones that never fill their own line, so a missing clear is
    /// invisible until an iteration is deep enough for a ply to be interior in
    /// one pass and a leaf in the next.
    #[test]
    fn finds_the_mate_in_one() {
        let (_, lines) = run("sfen 4k4/9/4G4/9/9/9/9/9/4K4 b G 1", depth(4));
        let last = lines.last().expect("the search reported something");
        assert_eq!(field(last, "mate"), 1, "{last}");
        assert_eq!(pv_of(last).len(), 1, "{last}");
    }

    /// Every move of the reported principal variation has to be playable, in
    /// order, from the position the search was given — and the move the search
    /// answers with has to be the head of it.
    ///
    /// This catches a line assembled out of the wrong plies, the classic bug in
    /// a ply-threaded principal variation, and one that looks entirely
    /// plausible in a GUI until somebody replays it.
    ///
    /// **The depth is load-bearing, and was measured rather than guessed.**
    /// Reversing `update_pv` and running this at depth 3 leaves it green: a
    /// three-move line read backwards still happened to replay on every fixture
    /// tried. A line has to be long enough for its own order to matter. That is
    /// also why this test is here rather than in the USI conformance suite,
    /// where `quit` arrives with the rest of the script and no dialogue ever
    /// gets past depth 1.
    ///
    /// Sabotage: append the child line before the move in `update_pv` and this
    /// fails. Dropping the `pv[ply].clear()` at node entry does **not** show up
    /// here — that one is caught by `finds_the_mate_in_one`, and finding out
    /// which of the two tests actually notices took making both mutations.
    ///
    /// **The replay is the part with teeth; the head comparison at the end is
    /// nearly free.** `best` is `self.pv[0][0]` and the printed line is
    /// `&self.pv[0]`, read one statement apart in the same iteration, so a
    /// reversed `update_pv` keeps them equal — that is precisely why the test
    /// this replaced was worthless on its own. It is kept because it is not
    /// *entirely* free: it fails the day `search` starts answering with
    /// something other than the head of the line it published, which is a real
    /// mistake and one E1's MultiPV work could make.
    #[test]
    fn the_reported_pv_is_playable() {
        for args in ["startpos", "startpos moves 7g7f 3c3d"] {
            let (best, lines) = run(args, depth(5));
            let last = lines.last().unwrap_or_else(|| panic!("nothing for {args}"));
            let pv = pv_of(last);
            assert!(pv.len() > 1, "a one-move line proves nothing: {last}");

            let mut replay = game(args);
            for token in &pv {
                replay
                    .push_usi_move(token)
                    .unwrap_or_else(|e| panic!("the pv in `{last}` is not playable: {e}"));
            }

            let BestMove::Play { mv, .. } = best else {
                panic!("{args} has legal moves")
            };
            assert!(is_legal(game(args).position(), mv), "{last}");
            assert_eq!(mv.to_usi_owned(), pv[0], "{last}");
        }
    }

    /// A reported mate has to be a mate on the board, not an artefact of the
    /// scoring.
    ///
    /// Sabotage: score a mated node `Score::mated_in(0)` instead of
    /// `mated_in(ply)` and the announced distance stops matching the line.
    #[test]
    fn a_reported_mate_is_a_real_mate() {
        let args = "sfen 4k4/9/4G4/9/9/9/9/9/4K4 b G 1";
        let (_, lines) = run(args, depth(4));
        let last = lines.last().expect("the search reported something");
        let plies = field(last, "mate");
        assert!(plies > 0, "{last}");

        let pv = pv_of(last);
        assert_eq!(i64::try_from(pv.len()).expect("short pv"), plies, "{last}");

        let mut replay = game(args);
        for token in pv {
            replay
                .push_usi_move(token)
                .unwrap_or_else(|e| panic!("the reported pv is not playable: {e}"));
        }
        assert!(
            !replay.position().has_legal_moves(),
            "the line the search called mate leaves legal moves"
        );
    }

    /// A depth nobody could reach, cut short by a deadline.
    ///
    /// Sabotage: drop the deadline arm from `Budget::expired` and this hangs
    /// rather than failing.
    #[test]
    fn a_deep_search_still_answers_a_deadline() {
        let limits = Limits {
            depth: Some(64),
            movetime: Some(Duration::from_millis(20)),
            ..Limits::default()
        };
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ =
                tx.send(NegamaxSearcher::new().search(&job(game("startpos"), limits), &SilentSink));
        });
        assert!(
            matches!(
                rx.recv_timeout(Duration::from_secs(10)),
                Ok(BestMove::Play { .. })
            ),
            "a 20 ms search did not answer within ten seconds"
        );
    }

    /// The same, by node count. The bound is loose on purpose: the poll fires
    /// every `POLL_INTERVAL_NODES`, so overshooting by less than one interval
    /// per frame on the way out is the guarantee, not exactness.
    ///
    /// Sabotage: drop the node-limit arm from `Budget::expired` and this times
    /// out rather than failing on the bound.
    #[test]
    fn a_deep_search_still_answers_a_node_limit() {
        let limits = Limits {
            depth: Some(64),
            nodes: Some(5_000),
            ..Limits::default()
        };
        // Off-thread with a timeout, like the deadline test above, so that a
        // search which never honours the limit fails here instead of wedging
        // the whole suite.
        let sink = Arc::new(Lines::default());
        let searching = Arc::clone(&sink);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(
                NegamaxSearcher::new().search(&job(game("startpos"), limits), searching.as_ref()),
            );
        });
        let best = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("a 5 000 node search did not answer within ten seconds");
        assert!(matches!(best, BestMove::Play { .. }));

        let lines = sink.take();
        let last = lines.last().expect("the search reported something");
        let nodes = field(last, "nodes");
        assert!(
            nodes < 5_000 + 4 * i64::try_from(POLL_INTERVAL_NODES).expect("fits"),
            "a 5 000 node search reported {nodes}"
        );
    }

    /// One line per iteration, deepening by one each time.
    ///
    /// The fixture states a depth and so is never cut short, which is the only
    /// case this covers. A search that *is* cut short still emits a line for the
    /// iteration it was in the middle of — see the comment beside `out.info` —
    /// so "one line per **completed** iteration" would be a stronger claim than
    /// either the code or this test makes.
    #[test]
    fn one_info_line_per_iteration() {
        let (_, lines) = run("startpos", depth(3));
        assert_eq!(lines.len(), 3, "{lines:?}");
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(field(line, "depth"), i as i64 + 1, "{line}");
            assert!(!pv_of(line).is_empty(), "{line}");
        }
    }

    /// The search has to give the shared buffer back.
    ///
    /// Forgetting `truncate` is a leak rather than a wrong answer — every ply
    /// reads its base before generating, so nobody ever sees another ply's
    /// moves — which is why the buffer's own tests cannot catch it and this
    /// one has to. Sabotage: delete `self.buf.truncate(base)` from `negamax`
    /// and the buffer comes back holding thousands of moves.
    #[test]
    fn the_move_buffer_comes_back_empty() {
        let mut searcher = NegamaxSearcher::new();
        let _ = searcher.search(&job(game("startpos"), depth(3)), &SilentSink);
        assert_eq!(searcher.buf.len(), 0);
    }

    /// Each iteration leaves its answer at the head of the root list, so the
    /// next one starts there.
    ///
    /// That is what makes an iteration cut short an improvement on its
    /// predecessor rather than a lottery over whatever prefix of the root list
    /// it reached. The fixture is chosen so the two disagree: the initial
    /// position's depth-1 answer is not its depth-3 answer, so a deleted
    /// re-seed leaves generation order at the head and this fails. It names no
    /// move — both sides of the comparison come from the engine.
    #[test]
    fn each_iteration_starts_from_the_last_answer() {
        let mut searcher = NegamaxSearcher::new();
        let best = searcher.search(&job(game("startpos"), depth(3)), &SilentSink);
        let BestMove::Play { mv, .. } = best else {
            panic!("the initial position has legal moves")
        };
        assert_eq!(
            searcher.root_moves[0], mv,
            "the root list was not re-seeded with the iteration's answer"
        );
    }

    /// Which budget decides how deep the deepening is allowed to go.
    ///
    /// [`DEFAULT_DEPTH`] answers "no budget of any kind was named", and only
    /// that. Sabotage: apply it as a ceiling over a stated budget as well — the
    /// shape this replaced, and a bug the unit tests above all missed because
    /// every one of them names a depth — and `go movetime 1000` thinks for four
    /// plies and hands the other 975 ms back.
    #[test]
    fn the_depth_ceiling_follows_the_budget_that_was_named() {
        let signals = SearchSignals::new();
        let started = Instant::now();
        let ceiling = |limits: Limits| Budget::new(&limits, &signals, started).max_depth;
        let second = Some(Duration::from_secs(1));

        assert_eq!(ceiling(Limits::default()), DEFAULT_DEPTH);
        // A clock with no per-move budget on it is still "nothing named":
        // turning `btime` into an allowance is step 5's whole subject.
        assert_eq!(
            ceiling(Limits {
                btime: second,
                wtime: second,
                byoyomi: Some(Duration::ZERO),
                ..Limits::default()
            }),
            DEFAULT_DEPTH
        );

        for limits in [
            Limits {
                movetime: second,
                ..Limits::default()
            },
            Limits {
                byoyomi: second,
                ..Limits::default()
            },
            Limits {
                nodes: Some(1_000),
                ..Limits::default()
            },
            Limits {
                infinite: true,
                ..Limits::default()
            },
        ] {
            assert_eq!(ceiling(limits), MAX_DEPTH, "{limits:?}");
        }

        // An explicit depth outranks everything, and is clamped at both ends.
        assert_eq!(
            ceiling(Limits {
                depth: Some(3),
                movetime: second,
                ..Limits::default()
            }),
            3
        );
        assert_eq!(ceiling(depth(0)), 1);
        assert_eq!(ceiling(depth(u32::MAX)), MAX_DEPTH);
    }

    /// …and the same thing end to end: a node budget worth several iterations
    /// gets spent on several iterations.
    #[test]
    fn a_stated_budget_deepens_past_the_default() {
        let limits = Limits {
            nodes: Some(300_000),
            ..Limits::default()
        };
        let (_, lines) = run("startpos", limits);
        let deepest = field(lines.last().expect("reported something"), "depth");
        assert!(
            deepest > i64::from(DEFAULT_DEPTH),
            "a 300 000 node budget only reached depth {deepest}"
        );
    }

    /// `go` with only a clock on it falls back to a fixed depth, and that depth
    /// is even — an odd one would end every unclocked line on an unanswered
    /// capture.
    #[test]
    fn an_unclocked_go_falls_back_to_an_even_default_depth() {
        assert_eq!(DEFAULT_DEPTH % 2, 0);
        let limits = Limits {
            btime: Some(Duration::from_secs(300)),
            wtime: Some(Duration::from_secs(300)),
            byoyomi: Some(Duration::ZERO),
            ..Limits::default()
        };
        let (best, lines) = run("startpos", limits);
        assert!(matches!(best, BestMove::Play { .. }));
        assert_eq!(
            i64::from(DEFAULT_DEPTH),
            field(lines.last().expect("reported something"), "depth")
        );
    }
}
