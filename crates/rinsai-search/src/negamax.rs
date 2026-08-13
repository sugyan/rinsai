//! Iterative-deepening negamax with alpha-beta pruning, over a quiescence
//! search that resolves captures.
//!
//! Three things to know before reading:
//!
//! * ⚠️ **There are two negation seams, not one.**
//!   [`NegamaxSearcher::child`] is the only place an *interior* node's child is
//!   searched and the only place the interior/quiescence dispatch happens, but
//!   [`NegamaxSearcher::qsearch`] recurses into itself and negates there — and
//!   that seam carries the great majority of the nodes. Anything that changes
//!   what happens at a negation has to touch both.
//! * The recursion carries a [`Window`] rather than a loose `(alpha, beta)`
//!   pair, so both child calls read `-self.…(board, -window, …)` and the classic
//!   `-search(-beta, -alpha)` transposition cannot be mistyped at either seam.
//! * The board is a **parameter**, never a field. `self.negamax(…)` while
//!   `&mut self.board` is live does not compile, and the tempting repairs
//!   (`RefCell`, `mem::take`) are all worse than passing it.
//! * ⚠️ **The repetition path is extended at one of those two seams and not
//!   the other**, which is the one asymmetry between them that is deliberate.
//!   [`NegamaxSearcher::path`] carries the argument.

use core::ops::{ControlFlow, Neg};
use std::time::Instant;

use shogi_core::Move;
use shunsai::Position;

use crate::eval;
use crate::game::HistoryEntry;
use crate::info::SearchInfo;
use crate::moves::{MAX_LEGAL_MOVES, MoveBuf};
use crate::repetition;
use crate::score::{Depth, MAX_PLY, Score};
use crate::search::{BestMove, InfoSink, Limits, SearchJob, SearchSignals, Searcher};
use crate::tt::{Bound, DEFAULT_HASH_MB, Hit, Table};

/// How deep a `go` that named no budget at all searches.
///
/// Four rather than six because there is no interior move ordering yet, so the
/// tree is close to a full minimax and each further pair of plies costs an
/// order of magnitude.
const DEFAULT_DEPTH: Depth = 4;

/// How often the search asks whether it is out of budget.
///
/// The conventional starting point, and untuned; step 5's SPRT is where it
/// stops being inherited.
///
/// ⚠️ **The test is an *exact* multiple, so every increment inside the tree has
/// to be seen by something that polls** — otherwise the counter steps clean
/// over a multiple and `stop`, the deadline and the node limit are all missed,
/// unpredictably rather than reliably. This is why
/// [`NegamaxSearcher::qsearch`] polls as well.
///
/// ⚠️ **The one exception is [`NegamaxSearcher::negamax_root`]'s own
/// increment.** `nodes` accumulates across iterations, so an iteration
/// beginning at `nodes ≡ 1023 (mod 1024)` consumes that multiple at the root
/// unpolled. Bounded and benign — every later increment in the iteration is
/// polled — but an exception to the sentence above, not an instance of it.
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
/// ⚠️ **It counts checks, not plies, and the two kinds of quiescence ply are
/// not alike.** A capture chain is self-limiting — every capture moves a piece
/// off the board and quiescence plays no drops, so occupancy strictly decreases
/// — and capping it is worse than unnecessary: a cap low enough to matter cuts
/// exchanges off in the middle, which is the horizon effect reintroduced two
/// plies down. A check-evasion chain has no such argument: an evasion may give
/// check back, need not be a capture, and its move list is *every* legal move
/// including drops. Left uncounted those chains dominate the whole search.
/// ⚠️ Two is the measured minimum that is also correct, and the cost is **not**
/// monotone in the cap — DECISIONS.md carries the sweep and why that surprises.
///
/// ⚠️ Evaluating a position that is still in check is a known lie, bounded by
/// how many times a line may be checked rather than by how long it is. E1's
/// check extension replaces it.
const QS_MAX_CHECK_PLIES: Depth = 2;

/// An alpha-beta window, negated as a unit.
#[derive(Clone, Copy, Debug)]
struct Window {
    alpha: Score,
    beta: Score,
}

impl Window {
    /// The full window, and ⚠️ **the only one
    /// [`NegamaxSearcher::negamax_root`] accepts** — it asserts as much, for
    /// the reason in that method's doc.
    fn open() -> Self {
        Self {
            alpha: -Score::INFINITE,
            beta: Score::INFINITE,
        }
    }
}

impl Neg for Window {
    type Output = Self;

    /// Swap and negate: the child's window is the parent's from the other side.
    fn neg(self) -> Self {
        Self {
            alpha: -self.beta,
            beta: -self.alpha,
        }
    }
}

/// Whether a transposition-table hit may be returned instead of searching.
///
/// Each arm returns a value on the far side of a window edge from the bound
/// that justifies it, so the node's fail-high or fail-low is proved.
///
/// ⚠️ **An exact score *strictly inside* the window may not be returned**, and
/// the reason is the principal variation rather than the score: cutting returns
/// after [`NegamaxSearcher::negamax`]'s `pv[ply].clear()` and before its move
/// loop, so the parent raises alpha on an empty line. Every other arm leaves the
/// parent failing high — its line then never surfaces — or untouched.
///
/// ⚠️ **That is a reading of the code, not an observation.** A sweep could not
/// reproduce it end to end; DECISIONS.md carries it and why the rule is kept.
fn cuts(hit: &Hit, window: &Window) -> bool {
    match hit.bound {
        Bound::Lower => hit.score >= window.beta,
        Bound::Upper => hit.score <= window.alpha,
        Bound::Exact => hit.score >= window.beta || hit.score <= window.alpha,
    }
}

/// What this search is allowed to spend.
///
/// Honours **a budget it was told** (`movetime`, `byoyomi`) and ignores **a
/// budget it would have to decide** (`btime` / `wtime` / `binc` / `winc`);
/// CONVENTIONS.md carries the rule and step 5 owns the second half.
struct Budget<'a> {
    signals: &'a SearchSignals,
    deadline: Option<Instant>,
    node_limit: Option<u64>,
    max_depth: Depth,
}

impl<'a> Budget<'a> {
    fn new(limits: &Limits, signals: &'a SearchSignals, started: Instant) -> Self {
        // `movetime` before `byoyomi`: a GUI that sends both means the
        // explicit one. `byoyomi 0` is "this time control has no byoyomi", not
        // a zero-millisecond budget.
        let stated = limits
            .movetime
            .or_else(|| limits.byoyomi.filter(|d| !d.is_zero()));
        // `infinite` means "until told to stop", which outranks any clock.
        let deadline = (!limits.infinite)
            .then_some(stated)
            .flatten()
            .map(|until| started + until);

        // `DEFAULT_DEPTH` answers "no budget of any kind was named" and only
        // that. Applying it as a ceiling over a stated budget too is the bug
        // this shape prevents: `go movetime 1000` would think for four plies
        // and hand the rest back.
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
    /// The first deepening iteration runs against this, which is what keeps
    /// "the search answers with a move it actually searched" true: a poll
    /// landing inside the first root move's subtree would leave
    /// [`NegamaxSearcher::negamax_root`] returning `None` and the answer sitting
    /// at the unsearched seed — a move in shunsai's unspecified generation
    /// order. CONVENTIONS.md carries the rule.
    ///
    /// ⚠️ **`signals.stopped()` stays live**, which is why this is a second
    /// budget rather than a flag that skips the poll: `stop` means quit, and
    /// [`Self::expired`] is where it is read.
    ///
    /// ⚠️ **It covers the whole first iteration, and only root move 0 needs
    /// it** — alpha is still `-INFINITE` there, so any finite score raises it.
    /// The rest is a bounded overrun, left standing because narrowing it
    /// changes what the engine does with a *clock*, which is step 5's subject.
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
/// driver's worker thread owns one for its whole life.
#[derive(Debug)]
pub struct NegamaxSearcher {
    /// The root's moves, kept out of [`Self::buf`] on purpose: they persist
    /// across iterations and get reordered, which is not what a ply-threaded
    /// buffer is for.
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
    /// Survives between searches on purpose — a position's value does not stop
    /// being true because the clock moved. `usinewgame` is what clears it.
    tt: Table,
    /// Every position on the way to the current node, oldest first: the game's
    /// own history, then one entry per move the search has played. What
    /// [`repetition::verdict`] reads.
    ///
    /// ⚠️ **Extended at interior nodes only, and [`Self::qsearch`] is a hole in
    /// it on purpose.** Quiescence is the overwhelming majority of all nodes,
    /// so keeping the push and the scan out of it is most of what the feature
    /// costs — quiescence is 91–99% of all nodes. Two things
    /// make the hole narrow: the position a quiescence subtree *starts* from is
    /// one its interior parent already pushed, and a quiescence line cannot
    /// come back to that starting position — every ply but at most
    /// [`QS_MAX_CHECK_PLIES`] of them is a capture, a capture takes a piece off
    /// the board, and the only move quiescence has that puts one back is a drop
    /// from an evasion.
    ///
    /// ⚠️ **What is left is a real gap, not a proof.** That argument bounds a
    /// quiescence line against its own entry; it says nothing about a
    /// quiescence position coinciding with one from the *game's* history, which
    /// would be a fourth occurrence this search does not see. It is the same
    /// family as the imprecisions [`Self::qsearch`]'s own doc lists, and E1
    /// closes it with them — the item that gives quiescence checks and
    /// non-capture promotions also ends the bound above, because a quiescence
    /// node can then play a quiet move.
    path: Vec<HistoryEntry>,
    nodes: u64,
    /// The deepest ply reached, quiescence included. Reset **per iteration**,
    /// unlike [`Self::nodes`], which accumulates across them — CONVENTIONS.md
    /// carries both rules and the asymmetry is deliberate.
    seldepth: usize,
    /// Sticky. Once the budget is spent every frame returns at once without
    /// polling again, so an abandoned subtree cannot spend time on its way out.
    stopped: bool,
}

impl NegamaxSearcher {
    /// A searcher whose table is the size `USI_Hash` advertises as its default.
    ///
    /// ⚠️ **Not what a test wants.** It allocates [`DEFAULT_HASH_MB`] MiB, and
    /// the suite builds a searcher per dialogue — use [`Self::with_hash_mb`]
    /// there.
    #[must_use]
    pub fn new() -> Self {
        Self::with_hash_mb(DEFAULT_HASH_MB)
    }

    /// Nodes visited by the last search. Caller: `rinsai bench`, which has no
    /// other way to read the figure it exists to freeze.
    #[must_use]
    pub fn nodes(&self) -> u64 {
        self.nodes
    }

    /// A searcher with a table of `mb` MiB.
    ///
    /// Callers: `rinsai bench`, and every test that does not want a quarter of a
    /// gigabyte. ⚠️ **Not `setoption name USI_Hash`** — that resizes the table
    /// the worker's searcher already owns, through [`Searcher::set_hash_mb`].
    #[must_use]
    pub fn with_hash_mb(mb: usize) -> Self {
        Self {
            root_moves: Vec::with_capacity(MAX_LEGAL_MOVES),
            buf: MoveBuf::new(),
            pv: (0..=MAX_PLY).map(|_| Vec::with_capacity(MAX_PLY)).collect(),
            tt: Table::new(mb),
            path: Vec::with_capacity(MAX_PLY),
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
    /// rather than defensive.** What it returns `None` on is "no root move
    /// raised alpha", which is the same as "no root move finished" only while
    /// alpha starts at `-INFINITE`. Given a narrow window, an ordinary
    /// **fail-low** comes back as `None` and [`Searcher::search`] breaks out of
    /// the deepening loop instead of re-searching — no wrong answer, no panic,
    /// and no failing test, because every test here searches an open window.
    /// It also ignores `window.beta` outright. E1 owns both, and owes this
    /// method a return type that tells fail-low apart from abandoned.
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
            self.path.push(HistoryEntry::of(board));
            let score = self.child(board, window, depth - 1, 1, budget);
            self.path.pop();
            board.undo_move(mv);

            // An abandoned subtree returns a placeholder, not a score.
            // Discard it before it can beat anything.
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
    /// ⚠️ **This is the interior seam, not the only one** — [`Self::qsearch`]
    /// negates inside itself as well.
    ///
    /// ⚠️ **This check covers interior-node children; [`Self::qsearch`] carries
    /// the matching one for the quiescence recursion, which never passes
    /// through here.** Forgetting a `truncate` is a leak rather than a wrong
    /// answer — every ply reads its own base, so the search still plays
    /// correctly — and the parent frame's own `truncate` hides the evidence
    /// before a test can reach it, which is why the assertion is at the
    /// boundary rather than in a test.
    ///
    /// ⚠️ **It is also where 千日手 is decided.** Both callers push
    /// [`Self::path`] immediately before calling this, so the position asked
    /// about is the one just reached. Every node below the root is entered
    /// through here, **the depth-1 iteration's children included** — that
    /// iteration dispatches straight into [`Self::qsearch`], so a check at the
    /// top of [`Self::negamax`] would leave the one search that always
    /// completes unable to see the move that loses the game.
    ///
    /// ⚠️ **Returning here keeps the verdict node's own key out of the table,
    /// and nothing more.** The caller takes the returned score as its `best`
    /// and stores *that* under a key with no path in it — CONVENTIONS.md
    /// carries the imprecision.
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
        let given_path = self.path.len();

        if let Some(rep) = repetition::verdict(&self.path) {
            // ⚠️ Nothing was searched at `ply`, so its line is whatever a
            // sibling left there. Without this the parent raises alpha and
            // `update_pv` grafts that stale tail on, publishing a variation
            // that runs past the end of the game.
            self.pv[ply].clear();
            // `rep` is from the side to move at the child, like every other
            // score crossing this seam.
            return -rep.score();
        }

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
        debug_assert_eq!(
            self.path.len(),
            given_path,
            "a child left the repetition path unbalanced"
        );
        score
    }

    /// An interior node. Not called `search`, because [`Searcher::search`] is
    /// the trait method on this same type.
    fn negamax(
        &mut self,
        board: &mut Position,
        mut window: Window,
        depth: Depth,
        ply: usize,
        budget: &Budget<'_>,
    ) -> Score {
        // Above every early return: one node per entry, including a node that
        // turns straight round. `bench` freezes these counts.
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

        // Kept for the store on the way out, because the loop below moves the
        // window's alpha and the bound is decided by comparing the two.
        let entry_alpha = window.alpha;
        let key = board.key();
        let hit = self.tt.probe(key, ply);
        if let Some(hit) = hit
            && hit.depth >= depth
            && cuts(&hit, &window)
        {
            return hit.score;
        }

        let base = self.buf.generate(board);
        if self.buf.len() == base {
            // Shogi has no stalemate, so no legal move is mate — no `in_check`
            // call needed. Distance is from the search root, not the remaining
            // depth, so a mate score means the same in every iteration.
            return Score::mated_in(ply);
        }

        // ⚠️ **Membership in the list this node generated is the legality
        // check**, and the only one: a key collision hands back a move from
        // another position, and shunsai's `do_move` validates nothing.
        if let Some(mv) = hit.and_then(|hit| hit.mv)
            && let Some(index) = (base..self.buf.len()).find(|&i| self.buf.get(i) == mv)
        {
            self.buf.swap(base, index);
        }

        let mut best = -Score::INFINITE;
        // Only a move that raised alpha, because only such a move was proved
        // better than something. A fail-low node offers none, and offering the
        // least bad of a losing set would fill the table with noise.
        let mut best_move = None;
        for i in base..self.buf.len() {
            let mv = self.buf.get(i);
            board.do_move(mv);
            self.path.push(HistoryEntry::of(board));
            let score = self.child(board, window, depth - 1, ply + 1, budget);
            // Both of these come before every `break` below, without exception.
            self.path.pop();
            board.undo_move(mv);

            if self.stopped {
                break;
            }
            if score > best {
                best = score;
                if score > window.alpha {
                    window.alpha = score;
                    best_move = Some(mv);
                    self.update_pv(ply, mv);
                    if window.alpha >= window.beta {
                        break;
                    }
                }
            }
        }
        self.buf.truncate(base);

        // ⚠️ **Nothing is stored from an abandoned search**, and not because
        // `best` is a placeholder — the `break` above discards those. It is that
        // `best` is then the maximum over a *prefix* of the list, so every bound
        // below would be false, and the entry outlives the search that made it.
        if !self.stopped {
            let bound = if best >= window.beta {
                Bound::Lower
            } else if window.alpha > entry_alpha {
                Bound::Exact
            } else {
                Bound::Upper
            };
            self.tt.store(key, ply, depth, bound, best, best_move);
        }
        best
    }

    /// Plays the exchanges out, so that no evaluation is ever taken in the
    /// middle of one.
    ///
    /// Without this a fixed-depth material search believes whatever the last
    /// ply happened to leave on the board — the horizon effect.
    ///
    /// **It searches captures, and nothing else** — deliberately an unordered,
    /// unpruned baseline for E1 to measure its additions against. ⚠️ It is
    /// therefore blind to と金作り, a large material event, until E1 adds
    /// non-capture promotions.
    ///
    /// ⚠️ **Two imprecisions kept, and neither is confined to one branch.**
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
    /// Both are the depth-zero imprecision narrowed, not removed.
    fn qsearch(
        &mut self,
        board: &mut Position,
        mut window: Window,
        depth: Depth,
        ply: usize,
        budget: &Budget<'_>,
    ) -> Score {
        // Same convention and same position as `negamax`: one node per entry.
        self.nodes += 1;
        self.pv[ply].clear();
        self.seldepth = self.seldepth.max(ply);

        if self.stopped {
            return Score::ZERO;
        }
        // ⚠️ Quiescence polls, and that is load-bearing — see
        // `POLL_INTERVAL_NODES`.
        if self.nodes.is_multiple_of(POLL_INTERVAL_NODES) && budget.expired(self.nodes) {
            self.stopped = true;
            return Score::ZERO;
        }

        // The same ply bound as `negamax`'s, not one short: both index `pv`
        // with the same counter and must agree where the stack ends. A backstop
        // rather than the working bound.
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
            // option, so a score assuming we could is a claim about a position
            // that does not exist. Generation is shunsai's full legal walk,
            // which restricts itself to evasions here, so an empty list means
            // mate exactly as it does at an interior node.
            base = self.buf.generate(board);
            if self.buf.len() == base {
                self.buf.truncate(base);
                return Score::mated_in(ply);
            }
            best = -Score::INFINITE;
        } else {
            // Standing pat assumes the side to move has *some* move worth at
            // least the static evaluation. An assumption, not a theorem: false
            // in zugzwang, which drops make effectively absent in shogi, and
            // false when every legal move is a capture, which is rare enough to
            // live with. It is also why a node not in check never claims mate —
            // it did not generate every move, so it does not know.
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
            // `depth` counts checked plies only, so a capture chain never runs
            // the counter down and terminates on material instead.
            let given = self.buf.len();
            let score = -self.qsearch(
                board,
                -window,
                depth - Depth::from(in_check),
                ply + 1,
                budget,
            );
            // The `child`-side twin of this assertion cannot see the
            // qsearch->qsearch recursion, so it needs its own.
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
    /// `infinite` is about when a search stops *searching* — the searcher's own
    /// business, which is why `resign` comes through here too. `ponder` is
    /// about when an answer may be *sent*, and the driver holds that rule on
    /// every searcher's behalf (see `search::worker`).
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
        // ⚠️ The table is *not* reset here — see the field. What this does is
        // age it, so entries from the previous `go` still answer probes but
        // lose every replacement contest against this search's own.
        self.tt.new_search();
        // Not redundant with the per-ply truncation: a panic caught by
        // `search::worker` unwinds past every one of those, and the worker
        // keeps this searcher for the next `go`.
        self.buf.clear();
        for line in &mut self.pv {
            line.clear();
        }

        // The game's own history is the front of the search's path — 千日手
        // counts positions reached in the game, not positions visited in the
        // tree, and at E0 depths the earlier occurrences are behind the root
        // rather than inside the tree. Reserved past the seed so a deep line
        // cannot reallocate mid-search.
        self.path.clear();
        self.path.extend_from_slice(job.game.history());
        self.path.reserve(MAX_PLY);

        self.root_moves.clear();
        let root_moves = &mut self.root_moves;
        let _ = board.generate_moves(|set| {
            root_moves.extend(set);
            ControlFlow::Continue(())
        });
        if self.root_moves.is_empty() {
            return self.finish(job, BestMove::Resign);
        }

        // Seeded so every path out of the loop has a legal move to report.
        // ⚠️ Which move this is, is shunsai's unspecified generation order —
        // nothing may depend on it.
        let mut best = self.root_moves[0];

        // What the first iteration runs against, so the answer is a move the
        // search looked at rather than the seed. `stop` still gets through.
        let first = budget.without_limits();

        // Stays 0 until an iteration gets all the way through the root list —
        // see `BestMove::Play`'s field for what a caller may conclude from it.
        let mut completed_depth: Depth = 0;

        for depth in 1..=budget.max_depth {
            // Per-iteration, unlike `self.nodes` — see the field's doc.
            self.seldepth = 0;

            let iteration_budget = if depth == 1 { &first } else { &budget };
            let Some(score) =
                self.negamax_root(&mut board, Window::open(), depth, iteration_budget)
            else {
                break;
            };
            // `negamax_root` leaves its root loop early on `self.stopped` and
            // on nothing else, and `self.stopped` is sticky for the whole
            // search — so a return through it with the flag still clear means
            // every root move was searched at this depth.
            if !self.stopped {
                completed_depth = depth;
            }
            // ⚠️ Taking the answer from a *partial* iteration is safe for
            // exactly one reason: the root list was re-seeded below with the
            // last iteration's move, so everything this iteration finished was
            // compared against it at this depth. The `score` beside it is
            // weaker than the move — over a prefix of the root list it is a
            // lower bound, not the iteration's value — and USI has no way to
            // say so.
            best = self.pv[0][0];
            out.info(
                &SearchInfo {
                    depth,
                    seldepth: self.seldepth,
                    score,
                    nodes: self.nodes,
                    hashfull: self.tt.hashfull(),
                    elapsed: started.elapsed(),
                    pv: &self.pv[0],
                }
                .to_string(),
            );

            // The next iteration starts from this one's answer, which is what
            // makes the partial-iteration read above safe. Root-only; not the
            // interior move ordering E1 is about.
            if let Some(index) = self.root_moves.iter().position(|mv| *mv == best) {
                self.root_moves.swap(0, index);
            }

            // A proven mate does not get better with depth.
            if score.is_mate() || self.stopped || budget.expired(self.nodes) {
                break;
            }
        }

        self.finish(
            job,
            BestMove::Play {
                mv: best,
                ponder: None,
                completed_depth,
            },
        )
    }

    fn new_game(&mut self) {
        self.tt.clear();
    }

    /// ⚠️ **Says so when the table is not the size that was asked for**, which
    /// happens both by rounding (`key & mask` needs a power of two) and by the
    /// allocator refusing. A declared option is a promise, and an engine
    /// running on a quarter of the memory its operator configured, silently, is
    /// how an SPRT result stops meaning anything.
    fn set_hash_mb(&mut self, mb: usize) {
        self.tt.resize(mb);
        let got = self.tt.mib();
        if got != mb {
            eprintln!("rinsai: USI_Hash {mb} MiB is not available; using {got} MiB");
        }
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

    /// ⚠️ **Never [`NegamaxSearcher::new`] in a test.** It allocates
    /// [`DEFAULT_HASH_MB`] MiB, and the suite builds a searcher per `run` — of
    /// which one test can make several. One MiB is the smallest `USI_Hash`
    /// admits, so it is also the size the engine has to work at.
    fn searcher() -> NegamaxSearcher {
        NegamaxSearcher::with_hash_mb(1)
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
        let best = searcher().search(&job(game(args), limits), &sink);
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

    /// The mate-in-1..5 suite's board, with White's hand left to the caller.
    ///
    /// The White king is alone on 1a. Three Black knights cover its three
    /// flight squares — 3c covers 2a, 3d covers 2b, 2d covers 1b — and a Black
    /// lance waits on 1i behind a White pawn on 1f. `1i1f` takes the pawn and
    /// checks down the file; the king cannot move, cannot capture and has no
    /// piece on the board to interpose, so White's every legal reply is a drop
    /// onto 1b–1e. The lance takes the dropped piece, checking again from one
    /// square nearer, until White runs out and the lance reaches 1b — where the
    /// knight on 2d is what stops the king taking it.
    ///
    /// So **one pawn in White's hand is one more move of mate**, which is what
    /// makes this a suite over *distances* rather than five unrelated
    /// positions.
    ///
    /// ⚠️ **The interposing pieces are pawns, and that is load-bearing rather
    /// than arbitrary.** Black captures whatever it interposes and may drop it
    /// straight back — and a gold or a silver dropped on 1b is mate on the
    /// spot, so with any other piece the ladder collapses to a shorter mate and
    /// the distances stop being the ones the suite is named for. A *pawn*
    /// dropped there would be 打ち歩詰め and is illegal, which is the one shogi
    /// rule that makes a ladder like this hold its length. Verified rather than
    /// argued: built with golds first, it answered mate 5 where mate 9 was
    /// intended.
    ///
    /// ⚠️ **It is one geometry with a parameter**, so it establishes that the
    /// search reports mate distances correctly and proves them, and it
    /// establishes nothing about the variety of mating *shapes*.
    /// `finds_the_mate_in_one`'s 頭金 is a drop mate and the only other shape
    /// here; the movegen that decides the rest is shunsai's, and differentially
    /// tested there.
    const MATE_LADDER: &str = "sfen 8k/9/6N2/6NN1/9/8p/9/9/K7L b";

    /// Mate in `plies` plies, reported and proved.
    ///
    /// Proving it is the second half and the one that can fail quietly: the
    /// line is replayed into a game the test builds itself and the final
    /// position must have no legal move. ⚠️ Naming a move instead would be
    /// asserting shunsai's unspecified generation order — CONVENTIONS.md.
    fn assert_mate_in(args: &str, plies: i64) {
        let (_, lines) = run(args, depth(u32::try_from(plies).expect("a short mate")));
        let last = lines.last().expect("the search reported something");
        assert_eq!(field(last, "mate"), plies, "{last}");
        assert_mate_is_real(args, last, plies);
    }

    /// The proving half: `line`'s principal variation is `plies` long, playable
    /// from `args`, and ends with the loser having no move.
    ///
    /// ⚠️ Naming a move instead would be asserting shunsai's unspecified
    /// generation order — CONVENTIONS.md.
    fn assert_mate_is_real(args: &str, line: &str, plies: i64) {
        let pv = pv_of(line);
        assert_eq!(i64::try_from(pv.len()).expect("short pv"), plies, "{line}");

        let mut replay = game(args);
        for token in pv {
            replay
                .push_usi_move(token)
                .unwrap_or_else(|e| panic!("the reported pv is not playable: {e}"));
        }
        assert!(
            !replay.position().has_legal_moves(),
            "the line the search called mate leaves legal moves: {line}"
        );
    }

    /// The suite DESIGN.md's E0 verification list asks for: a mate at every
    /// distance from one move to five, each found at exactly the depth it needs
    /// and each proved by replay.
    ///
    /// ⚠️ **The distance is asserted, not merely reported.** The deepening
    /// loop stops at the first iteration that returns a mate score, so a
    /// fixture holding a shorter mate than its row claims announces the
    /// shorter number and fails the equality below. The depth each row is
    /// searched at is the depth it needs, not what makes the assertion bite.
    ///
    /// Sabotage: score a mated node `Score::mated_in(0)` rather than
    /// `mated_in(ply)` **in [`Self::qsearch`]** — the **first** row fails, on
    /// `mate 0` where 1 was expected, and the loop stops there.
    ///
    /// ⚠️ **The same mutation in [`Self::negamax`] leaves the whole suite
    /// green, `bench` included** — no test here or anywhere reaches it. A mate
    /// that surfaces in a reported line is always resolved in quiescence
    /// first, because quiescence runs past the nominal depth and the deepening
    /// loop stops at the first iteration that returns a mate score. DECISIONS.md
    /// records it beside the same asymmetry in `pv[ply].clear()`.
    #[test]
    fn finds_a_mate_at_every_distance_from_one_to_five() {
        for (moves, hand) in [(1, "-"), (2, "p"), (3, "2p"), (4, "3p"), (5, "4p")] {
            let args = format!("{MATE_LADDER} {hand} 1");
            assert!(
                !game(&args).in_check(),
                "the attacker, not the defender, is to move at the root"
            );
            assert_mate_in(&args, 2 * moves - 1);
        }
    }

    /// A four-fold repetition reached by shuffling a rook and a king, with
    /// Black a rook up and White to move. Every White move but one leaves it a
    /// rook down; the one returns to the position the root is the third
    /// occurrence of.
    ///
    /// ⚠️ **The rook is what makes the tests below able to fail.** On a
    /// material-equal shuffle — the initial position answers this description
    /// too — a draw score and the evaluation are both zero, and the assertions
    /// would hold with the verdict deleted. Each test asserts the imbalance
    /// before it asserts anything else.
    const SHUFFLE_TO_A_DRAW: &str = "sfen 4k4/9/9/9/R8/9/9/9/4K4 b - 1 moves \
         9e8e 5a4a 8e9e 4a5a 9e8e 5a4a 8e9e 4a5a 9e8e 5a4a 8e9e";

    /// The same shuffle one cycle short, so the returning move makes the
    /// *third* occurrence rather than the fourth.
    const SHUFFLE_ONE_CYCLE_SHORT: &str = "sfen 4k4/9/9/9/R8/9/9/9/4K4 b - 1 moves \
         9e8e 5a4a 8e9e 4a5a 9e8e 5a4a 8e9e";

    /// 連続王手: a Black rook checks the bare White king down a file, the king
    /// steps aside, the rook checks down the next file, the king steps back.
    ///
    /// White is to move, in check, and a rook down — and its evasion makes the
    /// fourth occurrence, so Black, who gave every check, loses. The root
    /// position of the game is the one the repetition returns to, which is why
    /// the history is a whole number of cycles and one longer than the two
    /// above.
    const PERPETUAL_CHECK: &str = "sfen 4k4/9/9/9/4R4/9/9/9/K8 w - 1 moves \
         5a4a 5e4e 4a5a 4e5e 5a4a 5e4e 4a5a 4e5e 5a4a 5e4e 4a5a 4e5e";

    /// [`PERPETUAL_CHECK`]'s root position on its own, with no history to
    /// repeat.
    const PERPETUAL_CHECK_ROOT: &str = "sfen 4k4/9/9/9/4R4/9/9/9/K8 w - 1";

    /// The score of the last `info` line, in centipawns.
    fn cp_of(lines: &[String]) -> i64 {
        field(lines.last().expect("the search reported something"), "cp")
    }

    /// 千日手 is four occurrences of one position, counted over the game's own
    /// history rather than only over the search's path — the fourth is three
    /// quarters behind the root here.
    ///
    /// Sabotage: delete the `repetition::verdict` arm from `child` and the
    /// score falls back to the material balance asserted below.
    #[test]
    fn the_fourth_occurrence_of_a_position_is_a_draw() {
        let fixture = game(SHUFFLE_TO_A_DRAW);
        assert!(
            eval::evaluate(fixture.position()) < Score::cp(-900),
            "the fixture must leave the side to move a rook down"
        );
        let (_, lines) = run(SHUFFLE_TO_A_DRAW, depth(4));
        assert_eq!(cp_of(&lines), 0, "{lines:?}");
    }

    /// The third occurrence is not the fourth, so one cycle short of 千日手 the
    /// same position is worth exactly what the material says.
    ///
    /// Sabotage: count occurrences against `OCCURRENCES - 2` and this position
    /// starts reporting a draw the rules do not offer.
    #[test]
    fn the_third_occurrence_is_not_yet_a_repetition() {
        let (_, lines) = run(SHUFFLE_ONE_CYCLE_SHORT, depth(4));
        assert!(cp_of(&lines) < -900, "{lines:?}");
    }

    /// 連続王手の千日手 — the side that checked its way to the fourth
    /// occurrence loses, so the side being checked wins without a mate.
    ///
    /// ⚠️ The score asserted here is the one number that cannot have come from
    /// the evaluation: the side to move is a rook *down*, and it reports a win.
    #[test]
    fn the_side_that_gave_every_check_loses_the_repetition() {
        let fixture = game(PERPETUAL_CHECK);
        assert!(fixture.in_check(), "the fixture's side to move is in check");
        assert!(
            eval::evaluate(fixture.position()) < Score::ZERO,
            "the fixture must leave the side to move behind on material"
        );
        let (_, lines) = run(PERPETUAL_CHECK, depth(4));
        assert_eq!(
            cp_of(&lines),
            i64::from(Score::REPETITION.get()),
            "{lines:?}"
        );
    }

    /// The depth-1 iteration is the one that always completes, and every root
    /// child in it is a quiescence node — so this is what says the verdict is
    /// asked for at the dispatch rather than inside the interior node.
    ///
    /// Sabotage, verified on this fixture and impossible on a deeper one: move
    /// the verdict from `child` to the top of `negamax` and depth 1 reports the
    /// material balance, while every later iteration still reports the win.
    #[test]
    fn the_first_iteration_sees_the_repetition() {
        let (_, lines) = run(PERPETUAL_CHECK, depth(1));
        assert_eq!(field(&lines[0], "depth"), 1, "{lines:?}");
        assert_eq!(
            cp_of(&lines),
            i64::from(Score::REPETITION.get()),
            "{lines:?}"
        );
    }

    /// A published line stops at the repeating move: there is no game after it.
    ///
    /// Sabotage: drop `pv[ply].clear()` from `child`'s repetition arm and the
    /// line keeps whatever a sibling left below that ply, publishing a
    /// variation that runs past the end of the game.
    #[test]
    fn the_published_line_stops_at_the_repeating_move() {
        let (_, lines) = run(PERPETUAL_CHECK, depth(4));
        let last = lines.last().expect("the search reported something");
        assert_eq!(pv_of(last).len(), 1, "{last}");
    }

    /// The same position reached without the history that repeats it is an
    /// ordinary position, and says so.
    ///
    /// ⚠️ **This does not establish that the table is clean.** It reads the
    /// *root* score, and the root is the one node that neither probes nor
    /// stores, so a verdict-derived entry left at ply 1 or below would not show
    /// here. What it does catch is the verdict leaking through a shared
    /// searcher's table all the way to a fresh root; CONVENTIONS.md carries the
    /// leak that stays.
    #[test]
    fn a_repetition_verdict_does_not_survive_into_a_position_without_it() {
        let mut searcher = searcher();

        let sink = Lines::default();
        let _ = searcher.search(&job(game(PERPETUAL_CHECK), depth(4)), &sink);
        assert_eq!(
            cp_of(&sink.take()),
            i64::from(Score::REPETITION.get()),
            "the first search must reach the verdict, or the second proves nothing"
        );

        let sink = Lines::default();
        let _ = searcher.search(&job(game(PERPETUAL_CHECK_ROOT), depth(4)), &sink);
        let lines = sink.take();
        assert!(cp_of(&lines) < 0, "{lines:?}");
    }

    /// `completed_depth` counts iterations that got through the **whole** root
    /// list, so a budget that lands inside one leaves it out — which is the
    /// whole difference between it and the last `info` line's `depth`.
    ///
    /// ⚠️ **The cap is load-bearing and the assertion says so out loud.** It
    /// has to land *inside* an iteration; one that happened to fall on a
    /// boundary would leave this green against a `completed_depth` that never
    /// consulted `self.stopped` at all.
    ///
    /// Sabotage: drop the `if !self.stopped` guard around
    /// `completed_depth = depth` and the interrupted iteration is counted.
    #[test]
    fn completed_depth_leaves_out_the_iteration_a_budget_cut_short() {
        let args = "sfen l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1";
        let limits = Limits {
            depth: Some(6),
            nodes: Some(20_000),
            ..Limits::default()
        };
        let (best, lines) = run(args, limits);
        let published = field(
            lines.last().expect("the search reported something"),
            "depth",
        );
        let BestMove::Play {
            completed_depth, ..
        } = best
        else {
            panic!("the fixture has legal moves");
        };
        assert!(
            i64::from(completed_depth) < published,
            "the cap did not interrupt an iteration, so this fixture proves \
             nothing: published depth {published}, completed {completed_depth}"
        );
    }

    /// The other direction, and it needs its own test: a search nothing
    /// interrupts reports the iteration it actually finished.
    ///
    /// Sabotage: never assign `completed_depth` and it stays 0, which the test
    /// above cannot see — 0 is below the published depth there too.
    #[test]
    fn completed_depth_reaches_the_last_iteration_when_nothing_interrupts() {
        let args = "sfen l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1";
        let (best, lines) = run(args, depth(4));
        let published = field(
            lines.last().expect("the search reported something"),
            "depth",
        );
        let BestMove::Play {
            completed_depth, ..
        } = best
        else {
            panic!("the fixture has legal moves");
        };
        assert_eq!(published, 4, "the depth limit was not reached");
        assert_eq!(i64::from(completed_depth), published);
    }

    #[test]
    fn answers_with_a_legal_move() {
        let fixture = game("startpos");
        let (best, _) = run("startpos", depth(2));
        match best {
            BestMove::Play { mv, ponder, .. } => {
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
    /// ⚠️ The fixture has no legal move on purpose: on the initial position the
    /// "no answer within 200 ms" assertion passes because the search is still
    /// working, so it would stay green with the wait loop deleted — the whole
    /// thing it is here to catch. Sabotage: delete the loop in `finish` and
    /// `recv_timeout` below succeeds where it must time out.
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
            let _ = tx.send(searcher().search(&job, &SilentSink));
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
            let _ = tx.send(searcher().search(&job, &SilentSink));
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
    /// Asserted rather than assumed: a fixture that quietly acquired one would
    /// leave the equation below looking like a passing test of a convention it
    /// had stopped measuring.
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

    /// The frozen node-counting convention as an equation: one node for the
    /// root plus one for each move it tries.
    ///
    /// Both fixtures are capture-free one ply in, so every child stands pat
    /// immediately and the equation is exactly `1 + N`. ⚠️ The initial
    /// position qualifies for a non-obvious reason: `7g7f` opens *Black's*
    /// bishop diagonal, and White's own pawn on 3c blocks the bishop on 2b, so
    /// `2b8h+` is not available.
    ///
    /// Sabotage: move `self.nodes += 1` below the stand-pat return in
    /// `qsearch` and the count collapses. ⚠️ Not to 1 — the root window is
    /// open, so the first root move's child cannot take the stand-pat cutoff.
    #[test]
    fn the_root_and_every_leaf_count_as_one_node() {
        for args in [
            "startpos",
            // Two lone kings: the one fixture whose capture-free-ness is
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
    /// Sabotage: have `child` evaluate instead of dispatching to `qsearch`.
    ///
    /// ⚠️ **Making `generate_captures` return nothing does *not* fire it**,
    /// though it reads as the more direct mutation. A check is reachable one
    /// ply into this fixture, and a quiescence node in check takes the evasion
    /// branch — which generates every legal move and recurses whatever the
    /// capture filter does. So the count stays above `1 + N` and only the
    /// frozen `bench` counts move.
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

    /// Quiescence removed the horizon effect, as a falsifiable prediction.
    ///
    /// The threshold is the magnitude the fingerprint had — a pawn on the board
    /// plus a pawn in hand, reported at odd depths from the symmetric initial
    /// position — so the effect returning fails this whether or not it returns
    /// at the same depths. ⚠️ An upper bound and nothing more: a search that
    /// broke some new way and reported ±90 passes.
    ///
    /// Sabotage: evaluate in `child` instead of dispatching to `qsearch`.
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
    /// pruning is the widest it will ever be.
    ///
    /// The ceiling is measured and loose on purpose: it catches
    /// an unbounded recursion, not a number every future patch must update.
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
    /// ⚠️ **The fixture is load-bearing**: it is the only one found whose
    /// depth-1 iteration is expensive enough for the poll to fire inside it
    /// — 49 006 depth-1 nodes against at most 634 for the rest. Every other one
    /// makes this test unable to fail. `movetime 0` is the sharpest form of the
    /// question — the deadline has already passed when the search starts.
    ///
    /// Sabotage, verified on this fixture and on no other: pass `budget` rather
    /// than `first` to the depth-1 iteration and it fails on both counts at
    /// once — no `info` line at all, and the answer becomes whatever shunsai
    /// happened to generate first.
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

    /// A node limit has to be honoured where the unclocked first iteration is
    /// itself expensive — the interaction `a_deep_search_still_answers_a_node_limit`
    /// cannot reach, since depth 1 is cheap at the initial position.
    ///
    /// ⚠️ **Both bounds are built from a *measured* depth-1 cost, and the lower
    /// one is what makes the test load-bearing.** On a fixture whose depth 1 is
    /// cheaper than the limit, the limit bites in iteration *two* and deleting
    /// the `depth == 1` special case leaves this green.
    ///
    /// Sabotage: pass `budget` rather than `first` to the depth-1 iteration and
    /// this goes red either way round — the iteration is cut short, tripping
    /// the lower bound, or it stops before the first root move finishes and no
    /// `info` line is emitted at all.
    ///
    /// ⚠️ **It is not the test that catches the poll being dropped from
    /// `qsearch`.** That mutation goes red on
    /// `a_deep_search_still_answers_a_node_limit`, whose bound is far tighter.
    #[test]
    fn a_node_limit_is_honoured_inside_a_quiescence_subtree() {
        // The same fixture as `the_first_iteration_is_never_abandoned`, and
        // for the same reason.
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
    /// ⚠️ **The depth is not load-bearing.** The mate is found in the depth-1
    /// iteration — `quiescence_does_not_stand_pat_in_check` asserts that
    /// directly — so the deepening loop breaks straight after it. The depth is
    /// kept because it costs nothing and the assertion is about the answer, not
    /// the route.
    ///
    /// Sabotage: in **[`Self::qsearch`]**, score a mated node
    /// `Score::mated_in(0)` instead of `mated_in(ply)` and the announced
    /// distance is wrong, or drop its `pv[ply].clear()` and a stale line from
    /// an earlier subtree hangs off the mate. ⚠️ Making *either* mutation in
    /// [`Self::negamax`] instead changes nothing here or anywhere — DECISIONS.md
    /// records both halves of that asymmetry.
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
    /// This catches a line assembled out of the wrong plies — the classic
    /// ply-threaded-PV bug, and one that looks plausible in a GUI until
    /// somebody replays it. It lives here rather than in the conformance suite
    /// because no dialogue there gets past depth 1.
    ///
    /// Sabotage: append the child line before the move in `update_pv`, or drop
    /// **[`Self::qsearch`]'s** `pv[ply].clear()`. ⚠️ Not [`Self::negamax`]'s —
    /// that copy has no observable effect, which DECISIONS.md records.
    ///
    /// ⚠️ **The replay is the part with teeth; the head comparison at the end
    /// is nearly free.** `best` is `self.pv[0][0]` and the printed line is
    /// `&self.pv[0]`, read one statement apart, so a reversed `update_pv` keeps
    /// them equal. Kept because it still fails the day `search` answers with
    /// something other than the head of the line it published.
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
    /// `mated_in(ply)` **in [`Self::qsearch`]** and the announced distance
    /// stops matching the line. ⚠️ Not [`Self::negamax`]'s copy, which no test
    /// reaches — see DECISIONS.md.
    #[test]
    fn a_reported_mate_is_a_real_mate() {
        let args = "sfen 4k4/9/4G4/9/9/9/9/9/4K4 b G 1";
        let (_, lines) = run(args, depth(4));
        let last = lines.last().expect("the search reported something");
        let plies = field(last, "mate");
        assert!(plies > 0, "{last}");
        assert_mate_is_real(args, last, plies);
    }

    /// A depth nobody could reach, cut short by a deadline.
    ///
    /// Sabotage: drop the deadline arm from `Budget::expired`.
    ///
    /// ⚠️ **It fails rather than hanging, and that is what the off-thread
    /// `recv_timeout` above is for.** Something else in the suite does wedge on
    /// that mutation — `the_first_iteration_is_never_abandoned` searches on the
    /// calling thread — so a sweep that only watches the suite as a whole
    /// attributes the hang to the wrong test.
    #[test]
    fn a_deep_search_still_answers_a_deadline() {
        let limits = Limits {
            depth: Some(64),
            movetime: Some(Duration::from_millis(20)),
            ..Limits::default()
        };
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(searcher().search(&job(game("startpos"), limits), &SilentSink));
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
        // Off-thread with a timeout, so a search that never honours the limit
        // fails here instead of wedging the suite.
        let sink = Arc::new(Lines::default());
        let searching = Arc::clone(&sink);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(searcher().search(&job(game("startpos"), limits), searching.as_ref()));
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
    /// ⚠️ The fixture states a depth and so is never cut short, which is the
    /// only case this covers. A search that *is* cut short still emits a line
    /// for the iteration it was in the middle of, so "one line per **completed**
    /// iteration" would be a stronger claim than the code makes.
    #[test]
    fn one_info_line_per_iteration() {
        let (_, lines) = run("startpos", depth(3));
        assert_eq!(lines.len(), 3, "{lines:?}");
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(field(line, "depth"), i as i64 + 1, "{line}");
            assert!(!pv_of(line).is_empty(), "{line}");
        }
    }

    /// The search has to give the shared buffer back. ⚠️ Forgetting `truncate`
    /// is a leak rather than a wrong answer, which is why the buffer's own
    /// tests cannot catch it. Sabotage: delete `self.buf.truncate(base)` from
    /// `negamax` and the buffer comes back holding thousands of moves.
    #[test]
    fn the_move_buffer_comes_back_empty() {
        let mut searcher = searcher();
        let _ = searcher.search(&job(game("startpos"), depth(3)), &SilentSink);
        assert_eq!(searcher.buf.len(), 0);
    }

    /// Each iteration leaves its answer at the head of the root list, so the
    /// next one starts there.
    ///
    /// ⚠️ **Vacuous unless the answer differs from the move shunsai generates
    /// first**, which is why the first assertion is there — it fails loudly
    /// when the fixture stops discriminating, rather than leaving the second
    /// one passing for no reason. ⚠️ Only one of the five
    /// fixtures measured still discriminates, which is why this one is named.
    ///
    /// Sabotage: delete the `root_moves.swap` in the deepening loop.
    #[test]
    fn each_iteration_starts_from_the_last_answer() {
        let args = "sfen l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1";
        let mut generated = Vec::new();
        let _ = game(args).search_board().generate_moves(|set| {
            generated.extend(set);
            ControlFlow::Continue(())
        });

        let mut searcher = searcher();
        let best = searcher.search(&job(game(args), depth(3)), &SilentSink);
        let BestMove::Play { mv, .. } = best else {
            panic!("the fixture has legal moves")
        };
        assert_ne!(
            generated[0], mv,
            "the fixture's answer is now the first move generated, so this test \
             can no longer fail — find another"
        );
        assert_eq!(
            searcher.root_moves[0], mv,
            "the root list was not re-seeded with the iteration's answer"
        );
    }

    /// Which budget decides how deep the deepening is allowed to go.
    ///
    /// [`DEFAULT_DEPTH`] answers "no budget of any kind was named", and only
    /// that. Sabotage: apply it as a ceiling over a stated budget as well and
    /// `go movetime 1000` thinks for four plies and hands the rest back. ⚠️ No
    /// test that names a depth can catch this.
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

    // ----------------------------------------------------- transposition table

    /// One searcher of `mb` MiB, the same search twice, and the node count of
    /// each.
    ///
    /// One searcher, because what is measured is what the table carries between
    /// the two. ⚠️ **`mb` is a parameter because table *size* moves a node count
    /// on its own** — a bigger table evicts less — so comparing two searches at
    /// different sizes measures that instead.
    fn twice(
        mb: usize,
        args: &str,
        limits: Limits,
        between: impl FnOnce(&mut NegamaxSearcher),
    ) -> (i64, i64) {
        let mut searcher = NegamaxSearcher::with_hash_mb(mb);
        let first = Lines::default();
        let _ = searcher.search(&job(game(args), limits), &first);
        between(&mut searcher);
        let second = Lines::default();
        let _ = searcher.search(&job(game(args), limits), &second);

        let nodes =
            |lines: Vec<String>| field(lines.last().expect("an iteration finished"), "nodes");
        (nodes(first.take()), nodes(second.take()))
    }

    /// The table survives a `go`, so searching the same position again is
    /// cheaper.
    ///
    /// Sabotage: never store, or never probe — either makes the two counts
    /// equal. ⚠️ **Deleting the `buf.swap` does not fire this**, which is why
    /// the ordering has a test of its own below, on another fixture.
    #[test]
    fn a_repeated_search_reuses_what_the_last_one_proved() {
        let (first, second) = twice(1, "startpos", depth(6), |_| {});
        assert!(
            second < first,
            "the second search cost {second} against the first's {first}"
        );
    }

    /// The transposition move is searched first, as a node-count tripwire.
    ///
    /// A ceiling with room on both sides, like
    /// [`quiescence_is_bounded_on_the_drop_heavy_fixture`]. ⚠️ **The fixture is
    /// load-bearing** — the ordering is worth nothing at the initial position and
    /// up to 14.02× here; DECISIONS.md carries the sweep that picked it.
    ///
    /// Sabotage: drop the `buf.swap`.
    #[test]
    fn the_transposition_move_is_searched_first() {
        let (_, lines) = run("startpos moves 2g2f 8c8d 2f2e 8d8e", depth(5));
        let last = lines.last().expect("an iteration finished");
        assert!(
            field(last, "nodes") < 3_000_000,
            "the transposition move is not being tried first: {last}"
        );
    }

    /// …and `usinewgame` takes it away again, because the next game's positions
    /// are not merely stale — the entries describe a different tree.
    ///
    /// Sabotage: make `new_game` a no-op and the second count drops, exactly as
    /// in the test above.
    #[test]
    fn usinewgame_empties_the_table() {
        let (first, second) = twice(1, "startpos", depth(6), Searcher::new_game);
        assert_eq!(first, second, "entries survived `usinewgame`");
    }

    /// A resize does too — an operator changing the configuration mid-session
    /// must not leave the search reading entries from the old table.
    ///
    /// ⚠️ **The resize is to the size the searcher already has**, so what is
    /// measured is the clearing rather than the sizing: a different size also
    /// empties the table, and moves the count for its own reason.
    #[test]
    fn resizing_the_hash_empties_the_table() {
        for mb in [1, 2] {
            let (first, second) = twice(mb, "startpos", depth(6), |s| s.set_hash_mb(mb));
            assert_eq!(first, second, "entries survived a resize at {mb} MiB");
        }
    }

    /// `hashfull` is real data rather than a constant: zero while nothing has
    /// been stored, and above zero once the search has filled some of the table.
    ///
    /// ⚠️ **Depth 1 is genuinely zero**: the root dispatches its children
    /// straight into quiescence, which does not touch the table.
    ///
    /// ⚠️ **[`searcher`]'s one MiB is load-bearing for the second row**: at the
    /// advertised default an E0-depth search fills so little of the table that
    /// this reads zero permille, so the same assertion there could not pass.
    ///
    /// Sabotage: report a constant.
    #[test]
    fn hashfull_reports_the_table_filling_up() {
        let (_, lines) = run("startpos", depth(6));
        assert_eq!(field(&lines[0], "hashfull"), 0, "{}", lines[0]);
        let last = lines.last().expect("an iteration finished");
        assert!(field(last, "hashfull") > 0, "{last}");
        assert!(field(last, "hashfull") <= 1_000, "{last}");
    }

    /// The rule [`cuts`] exists for, as a table.
    ///
    /// ⚠️ **A unit test on purpose: no end-to-end test catches an in-window
    /// exact cut.** DECISIONS.md carries the sweep that failed to, and why the
    /// restriction is kept anyway.
    ///
    /// Sabotage: `Bound::Exact => true`.
    #[test]
    fn a_hit_cuts_only_where_it_lands_on_or_past_a_window_edge() {
        let window = Window {
            alpha: Score::cp(-50),
            beta: Score::cp(50),
        };
        let hit = |bound, cp| Hit {
            score: Score::cp(cp),
            depth: 0,
            bound,
            mv: None,
        };

        // Strictly inside: nothing may cut, an exact score included — this node
        // still owes its parent a line.
        for bound in [Bound::Upper, Bound::Lower, Bound::Exact] {
            for cp in [-49, 0, 49] {
                assert!(!cuts(&hit(bound, cp), &window), "{bound:?} at {cp} cp");
            }
        }
        // At or past an edge, each bound cuts on the side it proves and not on
        // the other. ⚠️ Inclusive: a score *equal* to β is a fail-high, which is
        // what makes this agree with the search's own `alpha >= beta`.
        for cp in [50, 51, 5_000] {
            assert!(cuts(&hit(Bound::Lower, cp), &window), "{cp} cp");
            assert!(cuts(&hit(Bound::Exact, cp), &window), "{cp} cp");
            assert!(!cuts(&hit(Bound::Upper, cp), &window), "{cp} cp");
        }
        for cp in [-50, -51, -5_000] {
            assert!(cuts(&hit(Bound::Upper, cp), &window), "{cp} cp");
            assert!(cuts(&hit(Bound::Exact, cp), &window), "{cp} cp");
            assert!(!cuts(&hit(Bound::Lower, cp), &window), "{cp} cp");
        }
    }

    /// `go` with only a clock on it falls back to a fixed depth.
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
