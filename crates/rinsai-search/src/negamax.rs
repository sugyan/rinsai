//! Iterative-deepening negamax with alpha-beta pruning.
//!
//! E0 step 3a's search: iterative deepening, alpha-beta, and a quiescence
//! search that resolves captures so that no evaluation is ever taken in the
//! middle of an exchange. Still absent, each with the step that owns it: the
//! transposition table and its move ordering (step 3b), the interior-node
//! ordering heuristics (E1), time *allocation* from the clock (step 5).
//!
//! The three structural decisions worth knowing before reading:
//!
//! * [`NegamaxSearcher::child`] is the only place an *interior* node's child is
//!   searched, and the only place the interior/quiescence dispatch happens —
//!   which is what keeps the node-counting convention exact.
//!   ⚠️ It is **not** the only place a score is negated:
//!   [`NegamaxSearcher::qsearch`] recurses into itself directly and negates
//!   there. There are two seams, not one, and by PROGRESS.md's generated-to-kept
//!   table the quiescence seam carries the overwhelming majority of the nodes —
//!   so anything that changes what happens at a negation (E1's extensions, an
//!   aspiration re-search, a ply or depth adjustment) has to touch both.
//! * The recursion carries a [`Window`] rather than a loose `(alpha, beta)`
//!   pair, so both child calls read `-self.…(board, -window, …)` and the classic
//!   `-search(-beta, -alpha)` transposition cannot be mistyped at either seam.
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
/// It no longer has to be even. Until step 3a it did: an odd depth ended the
/// line on a capture of ours that the opponent never answered, so the search
/// valued material it could not keep — which is exactly what quiescence
/// removes. The parity rule went with its reason (DECISIONS.md); the value
/// stays at four, because raising it is a behaviour change and E0 has no
/// instrument to justify one.
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
/// deadline at the node rates E0 has measured.
/// Expect the interval to buy less latency as a node gets more expensive —
/// ordering work at E1 — and expect step 5's time-management SPRT to be where
/// the number is chosen rather than inherited.
///
/// ⚠️ The test against it is an **exact** multiple, so every increment *inside
/// the tree* has to be seen by something that polls or the counter can step
/// clean over one. That is why [`NegamaxSearcher::qsearch`] polls as well:
/// counting a whole quiescence subtree without polling would make the values
/// seen at [`NegamaxSearcher::negamax`] entries non-consecutive by orders of
/// magnitude, and multiples could then be missed indefinitely.
///
/// The one increment that is *not* polled is
/// [`NegamaxSearcher::negamax_root`]'s own, so one multiple can be stepped over
/// per deepening iteration — `nodes` accumulates across iterations, so an
/// iteration beginning at `nodes ≡ 1023 (mod 1024)` consumes 1024 at the root.
/// That is bounded and benign: every later increment in the iteration is
/// polled, so the next multiple is at most one interval away. Named here
/// because it is an exception to the sentence above, not because it costs
/// anything.
const POLL_INTERVAL_NODES: u64 = 1024;

/// The deepest iteration any search will start.
///
/// One below [`MAX_PLY`] so that a line reaching the last iteration's nominal
/// depth still has a ply to sit at. Quiescence then runs *past* that depth, and
/// what stops it is the `ply >= MAX_PLY` guard both searches share — the spare
/// ply here is not what bounds it.
const MAX_DEPTH: Depth = MAX_PLY as Depth - 1;

/// How many **checked** plies one quiescence line may spend before it gives up
/// and evaluates where it stands.
///
/// It counts checks and not plies, and that distinction was measured rather
/// than reasoned — the numbers are in PROGRESS.md. The two kinds of quiescence
/// ply are not alike:
///
/// * A **capture** chain is self-limiting. Every capture takes a piece off the
///   board and into a hand, and quiescence plays no drops, so occupancy
///   strictly decreases and no shogi position affords more than about forty in
///   a row. Capping this is not merely unnecessary, it is harmful: a cap low
///   enough to matter cuts exchanges off in the middle, which is the horizon
///   effect quiescence exists to remove, reintroduced two plies further down.
/// * A **check-evasion** chain has no such argument. An evasion may give check
///   back and the reply may check again — 連続王手, whose rule does not arrive
///   until step 4 — and an evasion need not be a capture, so nothing decreases.
///   Worse, an evasion list is *every* legal move, drops included: a node an
///   order of magnitude wider than a capture node. Left uncounted, these chains
///   run to any depth they are allowed and dominate the whole search.
///
/// So only the checked plies are counted. Evaluating a position that is still
/// in check is a known lie, in the same family as step 2's depth-zero one; it
/// is now bounded by how many times a line may be checked rather than by how
/// long the line is, and E1's check extension is what replaces it.
const QS_MAX_CHECK_PLIES: Depth = 2;

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

    /// The same budget with the clock and the node limit suspended.
    ///
    /// The first deepening iteration runs against this, and that is what keeps
    /// "the search answers with a move it actually searched" true now that
    /// quiescence exists. Until step 3a the guarantee was arithmetic — a
    /// depth-1 iteration was `1 + N ≤ 594` nodes and the poll only fires on an
    /// exact multiple of 1024, so it could not fire. Quiescence removed the
    /// bound rather than the behaviour: a depth-1 iteration still costs a few
    /// hundred nodes in every fixture in this repository, and it took searching
    /// the drop-heavy fixture's descendants to find one — at 49 006 nodes —
    /// where the poll can fire at all. But *can* is enough, because a poll
    /// landing inside the first root move's subtree leaves
    /// [`NegamaxSearcher::negamax_root`] returning `None` and the answer sitting
    /// at the unsearched seed — a move in shunsai's unspecified generation
    /// order. CONVENTIONS.md carries the rule.
    ///
    /// ⚠️ **`signals.stopped()` stays live**, which is the whole reason this is
    /// a second budget rather than a flag that skips the poll: `stop` means
    /// quit, and [`Self::expired`] is where it is read. Skipping the poll would
    /// suspend `stop` along with the clock.
    ///
    /// ⚠️ **It covers the whole first iteration, and only the first root move
    /// needs it.** At root move 0 alpha is still `-INFINITE`, so any finite
    /// score raises it and the iteration has an answer from then on; every later
    /// move can be abandoned safely. Suspending the clock for all N of them
    /// instead is therefore an overrun the guarantee does not buy — bounded by
    /// one depth-1 iteration, and measured rather than estimated: PROGRESS.md
    /// carries the figure and the position it was taken on. Left as it is
    /// because narrowing it is a change to what the engine does with a *clock*,
    /// which is step 5's subject and its margin discipline, not a tidy-up to
    /// make on the way past.
    fn without_limits(&self) -> Self {
        Self {
            signals: self.signals,
            deadline: None,
            node_limit: None,
            max_depth: self.max_depth,
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
/// driver's worker thread owns one for its whole life. At step 3a that is still
/// only the allocations; step 3b's transposition table and E1's history tables
/// join them.
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
    /// The deepest ply reached, quiescence included.
    ///
    /// Reset **per iteration**, unlike [`Self::nodes`], which accumulates across
    /// them. The asymmetry is deliberate: USI prints `seldepth` beside `depth`
    /// and it means the selective depth *of that iteration*, while `nodes`
    /// accumulates because resetting it starves the poll in a sparse position.
    /// CONVENTIONS.md carries both rules.
    seldepth: usize,
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
            seldepth: 0,
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
            let score = self.child(board, window, depth - 1, 1, budget);
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

    /// Searches one child and negates, dispatching between the interior node
    /// and quiescence.
    ///
    /// The dispatch lives at the call site rather than at the top of
    /// [`Self::negamax`] for the counting convention's sake: one node per entry
    /// means each of the two must own its increment, and a `negamax` that
    /// turned round and called `qsearch` would count the node twice.
    ///
    /// ⚠️ **This is the interior seam, not the only one.** [`Self::qsearch`]
    /// recurses into itself and negates there, so `-search(-beta, -alpha)` is
    /// written out twice in this file. [`Window`] negating as a unit is what
    /// keeps both of them from being got wrong, and it is why the two calls read
    /// alike; it does not make this the single place to change.
    /// ⚠️ It also checks that a child gives the move buffer back exactly as it
    /// found it, and that check is here because nothing else can see it.
    /// Forgetting a `truncate` is a leak rather than a wrong answer — every ply
    /// reads its own base, so the search still plays correctly — and the parent
    /// frame's own `truncate` then hides the evidence before any test can
    /// reach it. `the_move_buffer_comes_back_empty` passes with quiescence's
    /// `truncate` deleted outright; the mutation was made and it stayed green,
    /// which is why the invariant is asserted at the boundary instead.
    #[inline]
    fn child(
        &mut self,
        board: &mut Position,
        window: Window,
        depth: Depth,
        ply: usize,
        budget: &Budget<'_>,
    ) -> Score {
        let given = self.buf.len();
        let score = if depth > 0 && ply < MAX_PLY {
            -self.negamax(board, -window, depth, ply, budget)
        } else {
            -self.qsearch(board, -window, 0, ply, budget)
        };
        debug_assert_eq!(
            self.buf.len(),
            given,
            "a child left {} moves in the buffer",
            self.buf.len() - given
        );
        score
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
        // turns straight round. Step 3b's `bench` freezes these counts.
        self.nodes += 1;
        self.pv[ply].clear();
        self.seldepth = self.seldepth.max(ply);

        // `child` dispatches everything else to `qsearch`, so arriving here at
        // all means there is a ply to sit at and a depth left to spend.
        debug_assert!(depth > 0 && ply < MAX_PLY);

        if self.stopped {
            return Score::ZERO;
        }
        if self.nodes.is_multiple_of(POLL_INTERVAL_NODES) && budget.expired(self.nodes) {
            self.stopped = true;
            return Score::ZERO;
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
            let score = self.child(board, window, depth - 1, ply + 1, budget);
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

    /// Plays the exchanges out, so that no evaluation is ever taken in the
    /// middle of one.
    ///
    /// Without this a fixed-depth material search believes whatever the last
    /// ply happened to leave on the board — the horizon effect, and the reason
    /// DESIGN.md puts quiescence in E0 rather than E1. PROGRESS.md carries the
    /// fingerprint it removes.
    ///
    /// **It searches captures, and nothing else.** Deliberately absent, each
    /// with the step that owns it: non-capture promotions (E1 item 8, beside
    /// SEE — と金作り is a large material event this is blind to, and the
    /// argument is in PROGRESS.md); checks (E1 item 8 as well, and they need
    /// `gives_check`, which shunsai does not have); SEE (E1 item 8, needs
    /// `attackers_to`); delta pruning (E1 item 9). What this produces is
    /// deliberately an *unordered, unpruned* baseline for each of those to be
    /// measured against.
    ///
    /// **Two imprecisions kept, and neither is confined to one branch.**
    ///
    /// * A node **not in check** never claims mate, cutoff or not. It only ever
    ///   generates captures, so an empty list means "no capture" and cannot be
    ///   told from "no legal move" — and the stand-pat β cutoff does not even
    ///   generate. Either way a position that is mate *without* check — legal in
    ///   shogi, and vanishingly rare — reports material.
    /// * A node **in check** past [`QS_MAX_CHECK_PLIES`] evaluates without
    ///   generating either, so a real checkmate there reports material too. That
    ///   is the known lie [`QS_MAX_CHECK_PLIES`]'s own doc admits, and it fires
    ///   far more often than the first.
    ///
    /// Both are step 2's depth-zero imprecision narrowed, not removed.
    fn qsearch(
        &mut self,
        board: &mut Position,
        mut window: Window,
        depth: Depth,
        ply: usize,
        budget: &Budget<'_>,
    ) -> Score {
        // The same convention as `negamax`, in the same position in the
        // function: one node per entry, counted before anything can return.
        self.nodes += 1;
        self.pv[ply].clear();
        self.seldepth = self.seldepth.max(ply);

        if self.stopped {
            return Score::ZERO;
        }
        // ⚠️ Quiescence polls, and that is load-bearing rather than tidy — see
        // `POLL_INTERVAL_NODES`. Counting nodes here without polling would let
        // the values seen at `negamax` entries skip a multiple of the interval
        // entirely, and `stop`, the deadline and the node limit would then be
        // missed unpredictably rather than reliably.
        if self.nodes.is_multiple_of(POLL_INTERVAL_NODES) && budget.expired(self.nodes) {
            self.stopped = true;
            return Score::ZERO;
        }

        // The same ply bound as `negamax`'s, not one short of it: `pv` has
        // `MAX_PLY + 1` rows and both searches index it with the same counter,
        // so they have to agree about where the stack ends. It is a backstop
        // rather than the working bound — a capture chain runs out of material
        // long before it, and a checked chain runs out of `QS_MAX_CHECK_PLIES`.
        if ply >= MAX_PLY {
            return eval::evaluate(board);
        }

        let in_check = board.in_check();
        if in_check && depth <= -QS_MAX_CHECK_PLIES {
            return eval::evaluate(board);
        }

        let base;
        let mut best;
        if in_check {
            // No stand-pat while in check: declining a check is not a legal
            // option, so a score that assumes we could is a claim about a
            // position that does not exist rather than a bound on this one.
            // Generation is shunsai's full legal walk, which restricts itself
            // to evasions here — so an empty list means mate, exactly as it
            // does at an interior node.
            base = self.buf.generate(board);
            if self.buf.len() == base {
                self.buf.truncate(base);
                return Score::mated_in(ply);
            }
            best = -Score::INFINITE;
        } else {
            // Standing pat is the claim that we need not capture at all: it
            // assumes the side to move has *some* move worth at least the
            // static evaluation, and takes that as a lower bound on the node.
            // That is the conventional quiescence assumption and it is an
            // assumption, not a theorem — it is false in zugzwang, and false in
            // a position whose only legal moves are captures. Drops make the
            // first effectively absent in shogi (DESIGN.md's E1 item 5 makes
            // the same argument for null-move pruning) and the second is rare
            // enough to live with. It is also what keeps a node that is not in
            // check from ever claiming mate: it did not generate every move, so
            // it does not know.
            let stand_pat = eval::evaluate(board);
            if stand_pat >= window.beta {
                return stand_pat;
            }
            if stand_pat > window.alpha {
                window.alpha = stand_pat;
            }
            best = stand_pat;
            base = self.buf.generate_captures(board);
        }

        for i in base..self.buf.len() {
            let mv = self.buf.get(i);
            board.do_move(mv);
            // `depth` counts checked plies only, so it moves only when this node
            // was one. A capture chain therefore never runs the counter down and
            // is left to terminate on material, which it does.
            let given = self.buf.len();
            let score = -self.qsearch(
                board,
                -window,
                depth - Depth::from(in_check),
                ply + 1,
                budget,
            );
            // The same boundary check as `child`'s, for the same reason: this is
            // the only place a quiescence child's leak is still visible.
            debug_assert_eq!(self.buf.len(), given, "a quiescence child leaked moves");
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
        self.seldepth = 0;
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

        // What the first iteration runs against instead, so that the answer is
        // always a move the search actually looked at rather than the seed. See
        // `Budget::without_limits` — `stop` still gets through it, so this
        // cannot delay a shutdown.
        let first = budget.without_limits();

        for depth in 1..=budget.max_depth {
            // `seldepth` is the selective depth *of this iteration*, so it
            // starts again here, and it is measured rather than seeded.
            // `self.nodes` deliberately does not reset — see the field's doc.
            self.seldepth = 0;

            let iteration_budget = if depth == 1 { &first } else { &budget };
            let Some(score) =
                self.negamax_root(&mut board, Window::open(), depth, iteration_budget)
            else {
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
                    seldepth: self.seldepth,
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

    /// Whether any root move hands the opponent a capture.
    ///
    /// The equation below holds only where the answer is no, and it is asserted
    /// rather than assumed: "no capture is reachable here" is a fact about a
    /// diagram, and a fixture that quietly acquired one would leave the equation
    /// looking like a passing test of a convention it had stopped measuring.
    fn a_capture_is_reachable_in_one_ply(args: &str) -> bool {
        let mut board = game(args).search_board();
        board.legal_moves().into_iter().any(|mv| {
            board.do_move(mv);
            let reachable = board
                .legal_moves()
                .into_iter()
                .any(|reply| board.piece_at(reply.to()).is_some());
            board.undo_move(mv);
            reachable
        })
    }

    /// The frozen node-counting convention, written as an equation: one node
    /// for the root plus one for each move it tries.
    ///
    /// Both fixtures are capture-free one ply in, so every child is a
    /// quiescence node that stands pat immediately and the equation is still
    /// exactly `1 + N`. The initial position keeps its place here for a reason
    /// worth recording, because it is not obvious and was got wrong while this
    /// step was being planned: `7g7f` opens *Black's* bishop diagonal, not
    /// White's — White's own pawn on 3c blocks the bishop on 2b, so `2b8h+` is
    /// not available and no root move offers a capture at all.
    ///
    /// Sabotage: move `self.nodes += 1` below the stand-pat return in `qsearch`
    /// and this drops to 1.
    #[test]
    fn the_root_and_every_leaf_count_as_one_node() {
        for args in [
            "startpos",
            // Two lone kings — the sparse position PROGRESS.md measured poll
            // starvation on, and the one fixture whose capture-free-ness is
            // structural rather than incidental.
            "sfen 4k4/9/9/9/9/9/9/9/4K4 b - 1",
        ] {
            assert!(
                !a_capture_is_reachable_in_one_ply(args),
                "the fixture stopped being capture-free, so this no longer measures the convention: {args}"
            );
            let expected = 1 + game(args).position().legal_moves().len() as i64;
            let (_, lines) = run(args, depth(1));
            assert_eq!(lines.len(), 1, "{args}");
            assert_eq!(field(&lines[0], "nodes"), expected, "{args}");
        }
    }

    /// The counterpart: where captures *are* reachable the equation has to
    /// break upward, because quiescence went and looked.
    ///
    /// Sabotage: make `generate_captures` return nothing, or have `child`
    /// evaluate instead of dispatching to `qsearch`, and this falls back to
    /// exactly `1 + N`.
    #[test]
    fn quiescence_resolves_captures_the_horizon_would_have_hidden() {
        let args = "sfen l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1";
        assert!(a_capture_is_reachable_in_one_ply(args));
        let flat = 1 + game(args).position().legal_moves().len() as i64;
        let (_, lines) = run(args, depth(1));
        assert!(
            field(&lines[0], "nodes") > flat,
            "quiescence searched nothing: {}",
            lines[0]
        );
    }

    /// **The step's headline claim, as a falsifiable prediction.**
    ///
    /// PROGRESS.md recorded the horizon effect as a fingerprint: from the
    /// initial position the search reports one pawn's board value plus one
    /// pawn's hand value — 215 — at odd depths and 0 at even ones, because an
    /// odd depth ends just after the engine takes a pawn and the recapture is
    /// past its horizon. The initial position is symmetric, so a score of that
    /// magnitude is the effect and nothing else.
    ///
    /// Written against the recorded number rather than a round one, so the
    /// threshold means something: it is the magnitude the fingerprint had, so
    /// the effect coming back fails this whether or not it comes back at the
    /// same depths. ⚠️ It is still an upper bound and nothing more — a search
    /// that broke in some new way and reported ±90 passes. Only the fingerprint
    /// is pinned here; the scores themselves are not.
    ///
    /// Sabotage: evaluate in `child` instead of dispatching to `qsearch`, and
    /// 215 comes straight back at every odd depth.
    #[test]
    fn the_horizon_effect_fingerprint_is_gone() {
        // 215 = a pawn on the board plus a pawn in hand, the two values the
        // fingerprint was made of.
        const FINGERPRINT: i64 = 215;
        for d in 1..=6 {
            let (_, lines) = run("startpos", depth(d));
            let last = lines.last().expect("an iteration finished");
            assert!(
                field(last, "cp").abs() < FINGERPRINT,
                "depth {d} still reports the horizon effect: {last}"
            );
        }
    }

    /// Declining a check is not a legal option, so a quiescence node in check
    /// has to generate — and finding nothing means mate, not material.
    ///
    /// Sabotage: stand pat unconditionally (drop the `in_check` branch) and the
    /// mate one ply away is reported as a material balance. It is the single
    /// worst thing a quiescence search can get wrong.
    #[test]
    fn quiescence_does_not_stand_pat_in_check() {
        // 頭金 again, but searched at depth 1 so that the mate has to be found
        // *inside* a quiescence node rather than at an interior one.
        let (_, lines) = run("sfen 4k4/9/4G4/9/9/9/9/9/4K4 b G 1", depth(1));
        let last = lines.last().expect("an iteration finished");
        assert_eq!(field(last, "mate"), 1, "{last}");
    }

    /// A quiet node with no captures stands pat. It must never report mate: it
    /// did not generate every move, so it does not know.
    ///
    /// Sabotage: return `Score::mated_in(ply)` when the *capture* list is empty
    /// rather than only when the evasion list is, and the engine announces mate
    /// in every quiet position it reaches — a mate it invented.
    #[test]
    fn quiescence_only_claims_mate_when_it_generated_every_move() {
        for d in 1..=4 {
            let (_, lines) = run("sfen 4k4/9/9/9/9/9/9/9/4K4 b - 1", depth(d));
            let last = lines.last().expect("an iteration finished");
            assert!(
                !last.contains(" score mate "),
                "quiescence invented a mate at depth {d}: {last}"
            );
        }
    }

    /// `seldepth` is the deepest ply an iteration reached. Where captures exist
    /// it must exceed the nominal depth, and where none do it must equal it —
    /// which is what makes it data rather than a copy of `depth`.
    ///
    /// Sabotage: feed `seldepth` from `depth`, or never update it in `qsearch`,
    /// and the tactical half fails.
    #[test]
    fn seldepth_reaches_past_the_nominal_depth_where_captures_exist() {
        let (_, quiet) = run("sfen 4k4/9/9/9/9/9/9/9/4K4 b - 1", depth(3));
        let last = quiet.last().expect("an iteration finished");
        assert_eq!(field(last, "seldepth"), field(last, "depth"), "{last}");

        let (_, tactical) = run(
            "sfen l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1",
            depth(3),
        );
        let last = tactical.last().expect("an iteration finished");
        assert!(
            field(last, "seldepth") > field(last, "depth"),
            "quiescence never went past the nominal depth: {last}"
        );
    }

    /// The explosion tripwire. Quiescence with no ordering, no SEE and no delta
    /// pruning is the widest it will ever be, and the drop-heavy fixture is
    /// where that shows first.
    ///
    /// The ceiling is measured rather than reasoned — the figure it was set
    /// from is in PROGRESS.md — and it is loose on purpose: it exists to catch
    /// an unbounded recursion, not to pin a number every future search patch
    /// would have to come back and update.
    #[test]
    fn quiescence_is_bounded_on_the_drop_heavy_fixture() {
        let (_, lines) = run(
            "sfen l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1",
            depth(4),
        );
        let last = lines.last().expect("an iteration finished");
        assert!(
            field(last, "nodes") < 20_000_000,
            "quiescence ran away: {last}"
        );
    }

    /// The first iteration is never abandoned, so the answer is always a move
    /// the search actually looked at.
    ///
    /// Until quiescence this was arithmetic — a depth-1 iteration was at most
    /// `1 + 593` nodes and the poll only fires on an exact multiple of 1024, so
    /// it could not fire. Quiescence removed the bound, not the behaviour:
    /// depth 1 still costs only a few hundred nodes in most positions, and it
    /// took a search over the drop-heavy fixture's descendants to find one where
    /// it does not. **The fixture below costs about forty-nine thousand nodes at
    /// depth 1**, which is what makes this test able to fail at all; the
    /// positions tried first cost 31 to 634 and could not.
    ///
    /// `movetime 0` is the sharpest form of the question: the deadline has
    /// already passed when the search starts.
    ///
    /// Sabotage, verified on this fixture and on no other: pass `budget` rather
    /// than `first` to the depth-1 iteration and it fails on both counts at once
    /// — no `info` line is emitted at all, and the answer becomes whatever
    /// shunsai happened to generate first.
    #[test]
    fn the_first_iteration_is_never_abandoned() {
        // Two plies on from the drop-heavy fixture, found by searching its
        // descendants for the most expensive depth-1 iteration.
        let args = "sfen l6nl/6+Pgk/2np1S3/p1p1p2Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn4p 3";
        let (best, lines) = run(
            args,
            Limits {
                movetime: Some(Duration::ZERO),
                ..Limits::default()
            },
        );
        let first = lines.first().expect("depth 1 emitted no info line");
        assert_eq!(field(first, "depth"), 1, "{first}");
        let pv = pv_of(first);
        assert!(!pv.is_empty(), "{first}");
        match best {
            BestMove::Play { mv, .. } => assert_eq!(mv.to_usi_owned(), pv[0]),
            BestMove::Resign => panic!("a position with legal moves resigned"),
        }
    }

    /// A node limit has to be honoured on a tactical position too, where the
    /// unclocked first iteration is itself expensive.
    ///
    /// Both bounds are built from a *measured* depth-1 cost, because the first
    /// iteration runs unclocked by design: nothing can stop before it finishes,
    /// so the limit can only bite after that. That interaction — an
    /// unstoppable iteration in front of a node limit — is what this covers and
    /// what the startpos-based `a_deep_search_still_answers_a_node_limit` does
    /// not, since depth 1 costs 31 nodes there.
    ///
    /// ⚠️ **The fixture has to be one whose depth-1 iteration outruns the
    /// limit, and the lower bound is what makes it load-bearing.** This test
    /// used the drop-heavy fixture itself, whose depth 1 costs 280 nodes
    /// against a limit of 20 000 — so the limit bit in iteration *two* and the
    /// interaction above went unmeasured: deleting the `depth == 1` special
    /// case in `Searcher::search` left it green. Measured, so that the fixture
    /// is chosen rather than assumed: with `nodes 20 000` the position below
    /// reports 49 006 nodes and one `info` line, i.e. exactly the unclocked
    /// first iteration and nothing after it.
    ///
    /// Sabotage: pass `budget` rather than `first` to the depth-1 iteration and
    /// this goes red either way round — the iteration stops at about 20 480
    /// nodes, which trips the lower bound, or it stops before the first root
    /// move finishes, in which case no `info` line is emitted at all.
    ///
    /// ⚠️ **It is not the test that catches the poll being dropped from
    /// `qsearch`.** That mutation was made, and the one that went red was
    /// `a_deep_search_still_answers_a_node_limit`, whose bound is far tighter.
    /// The note said otherwise until the mutation was actually run.
    #[test]
    fn a_node_limit_is_honoured_inside_a_quiescence_subtree() {
        // The same fixture as `the_first_iteration_is_never_abandoned` above,
        // and for the same reason: it is the one whose depth-1 iteration is
        // expensive enough for a stated budget to have something to cut.
        let args = "sfen l6nl/6+Pgk/2np1S3/p1p1p2Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn4p 3";
        let (_, first) = run(args, depth(1));
        let unclocked = field(&first[0], "nodes");

        let (_, lines) = run(
            args,
            Limits {
                depth: Some(64),
                nodes: Some(20_000),
                ..Limits::default()
            },
        );
        let nodes = field(lines.last().expect("an iteration finished"), "nodes");
        assert!(
            nodes >= unclocked,
            "the node limit cut the first iteration short: {nodes} < {unclocked}"
        );
        assert!(
            nodes < unclocked + 20_000 + 4 * i64::try_from(POLL_INTERVAL_NODES).expect("fits"),
            "the node limit went unnoticed inside quiescence: {nodes}"
        );
    }

    /// 頭金: a gold dropped on 5b, protected by the gold on 5c, with every
    /// flight square from 5a covered.
    ///
    /// The depth is no longer what makes this work, and the change is worth
    /// recording rather than quietly editing. Until step 3a a mate one ply away
    /// was invisible at depth 1, because a depth-zero node reported material
    /// without generating moves; the fixture ran at depth 4 for that reason.
    /// Quiescence removed it — the mate is now found in the depth-1 iteration,
    /// which `quiescence_does_not_stand_pat_in_check` asserts directly — so the
    /// deepening loop breaks straight after the first iteration and this is a
    /// one-iteration test. The depth is kept because it costs nothing and
    /// because the assertion is about the answer, not the route.
    ///
    /// Sabotage, both re-verified after quiescence landed: score a mated node
    /// `Score::mated_in(0)` instead of `mated_in(ply)` and the announced
    /// distance is wrong; drop the `pv[ply].clear()` at node entry and a stale
    /// line from an earlier subtree hangs off the mate. The *reason* the second
    /// one fires has changed with the search — it used to need an iteration
    /// deep enough for a ply to be interior in one pass and a leaf in the next,
    /// and it now fires at depth 1 because the quiescence leaf never fills its
    /// own line either.
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
    /// **The depth stopped being load-bearing at step 3a, and the measurement
    /// behind it has been re-run rather than inherited.** Step 2 measured that
    /// reversing `update_pv` and running this at depth 3 left it green — a
    /// three-move line read backwards still happened to replay on every fixture
    /// tried — and chose depth 5 for that reason. Re-measured with quiescence in
    /// place, **depth 3 now fails too**: quiescence lengthens the reported line
    /// well past the nominal depth, so it is long enough for its own order to
    /// matter much sooner. Depth 5 is kept because it costs nothing and searches
    /// more, not because 3 would no longer do.
    ///
    /// The test is still here rather than in the USI conformance suite, and that
    /// part is unchanged: `quit` arrives with the rest of the script there and
    /// no dialogue ever gets past depth 1.
    ///
    /// Sabotage: append the child line before the move in `update_pv` and this
    /// fails. Dropping the `pv[ply].clear()` at node entry **now fails here
    /// too**, and it did not before quiescence — the note used to say so and to
    /// point at `finds_the_mate_in_one` as the only test that noticed. Both
    /// mutations were re-run after quiescence landed; three tests catch the
    /// second one now, because a quiescence leaf leaves a line behind at plies
    /// that used to be evaluated without ever being entered.
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

    /// `go` with only a clock on it falls back to a fixed depth.
    ///
    /// It used to also assert that the depth was *even*, because an odd one
    /// ended every unclocked line on an unanswered capture. That was true only
    /// while there was no quiescence search, which is precisely what quiescence
    /// removes — so the assertion went with its reason (DECISIONS.md) rather
    /// than being left to pin a rule nobody could still justify.
    #[test]
    fn an_unclocked_go_falls_back_to_the_default_depth() {
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
