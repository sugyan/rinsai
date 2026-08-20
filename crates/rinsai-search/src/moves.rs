//! Move helpers that shunsai deliberately does not provide.

use core::ops::ControlFlow;

use shogi_core::Move;
use shunsai::{MoveSet, Position};

use crate::ordering;
use crate::score::MAX_PLY;

/// The most legal moves any shogi position has.
///
/// It is the size a move buffer has to be, and shunsai's own benches use the
/// same number (`benches/suite/common.rs` in its repository — the published
/// crate ships `src/` only).
pub const MAX_LEGAL_MOVES: usize = 593;

/// One move list, shared by every ply of one search.
///
/// [`Position::generate_moves`] takes `&self`, so nothing can play a move from
/// inside its callback: anything recursive has to collect first, and this is
/// where the collection goes. The shape — one allocation of
/// `MAX_LEGAL_MOVES * MAX_PLY`, sliced per ply, generation appending and the
/// caller truncating back on the way out — is shunsai's own
/// `perft_materialize` (`examples/perft.rs` in its repository, not the
/// published crate).
///
/// **The moves come out by value, one at a time, and that is load-bearing.**
/// A `&[Move]` handed back here would borrow the buffer for as long as the
/// caller iterates, and the caller wants to recurse through `&mut self` in the
/// middle of that loop. Indexing copies a `Move` — small and [`Copy`] — and
/// ends the borrow on the same line.
#[derive(Debug)]
pub(crate) struct MoveBuf {
    moves: Vec<Move>,
}

impl MoveBuf {
    /// Reserves the whole search's worth up front — one allocation for the
    /// life of the searcher, and none on any node.
    pub(crate) fn new() -> Self {
        Self {
            moves: Vec::with_capacity(MAX_LEGAL_MOVES * MAX_PLY),
        }
    }

    /// Appends every legal move in `position`, and returns the index the
    /// caller must [`truncate`](Self::truncate) back to.
    pub(crate) fn generate(&mut self, position: &Position) -> usize {
        let base = self.moves.len();
        let moves = &mut self.moves;
        let _ = position.generate_moves(|set| {
            moves.extend(set);
            ControlFlow::Continue(())
        });
        self.debug_assert_reserved();
        base
    }

    /// Appends every legal move whose destination holds an enemy piece, and
    /// returns the index the caller must [`truncate`](Self::truncate) back to.
    ///
    /// Quiescence's generator. shunsai has no captures-only generation, so the
    /// full legal walk still runs; what does **not** run is materialisation,
    /// since a [`MoveSet`] hands over its destinations as
    /// [`Bitboard`](shunsai::Bitboard)s.
    ///
    /// The result is a subset of [`Self::generate`]'s, so one ply still cannot
    /// exceed [`MAX_LEGAL_MOVES`].
    pub(crate) fn generate_captures(&mut self, position: &Position) -> usize {
        let base = self.moves.len();
        // ⚠️ Read before any move is made: after `do_move` the side to move
        // has flipped, so `them` becomes *our* pieces including the one just
        // moved, and every move looks like a capture — quiescence would never
        // terminate. Intersecting with our own board fails the other way: a
        // legal move never lands on our own piece, so nothing is generated and
        // quiescence stands pat everywhere, silently restoring the horizon
        // effect. Both are silent; neither is a crash.
        let them = position.player_bb(position.side_to_move().flip());
        let moves = &mut self.moves;
        let _ = position.generate_moves(|set| {
            // Exhaustive rather than `if let`, with the discarded arm spelled
            // out: a `MoveSet` variant added upstream would otherwise silently
            // go uncaptured — a strength bug with no symptom — instead of a
            // compile error naming the place to decide.
            match set {
                MoveSet::Normal {
                    from,
                    promotions,
                    non_promotions,
                    ..
                } => {
                    // The two boards overlap where promotion is optional, so a
                    // capture that may promote is emitted both ways. Where it
                    // is compulsory the square is in `promotions` alone.
                    for to in promotions & them {
                        moves.push(Move::Normal {
                            from,
                            to,
                            promote: true,
                        });
                    }
                    for to in non_promotions & them {
                        moves.push(Move::Normal {
                            from,
                            to,
                            promote: false,
                        });
                    }
                }
                // A drop can never capture — shunsai masks drop targets with
                // the empty squares — so the whole fan-out goes at once. That
                // is most of what a shogi move list is.
                MoveSet::Drop { .. } => {}
            }
            ControlFlow::Continue(())
        });
        self.debug_assert_reserved();
        base
    }

    /// Fails if the buffer has outgrown the reservation [`Self::new`] made,
    /// which means a ply generated twice or the search ran past [`MAX_PLY`].
    /// ⚠️ In release the symptom is a reallocation on the hot path, not a
    /// wrong answer.
    fn debug_assert_reserved(&self) {
        debug_assert!(
            self.moves.len() <= MAX_LEGAL_MOVES * MAX_PLY,
            "the move buffer outgrew its reservation at {} moves",
            self.moves.len()
        );
    }

    pub(crate) fn len(&self) -> usize {
        self.moves.len()
    }

    pub(crate) fn get(&self, index: usize) -> Move {
        self.moves[index]
    }

    /// Exchanges two moves. The transposition move's ordering: it is found by
    /// scanning the list a node generated anyway, then swapped to the front.
    pub(crate) fn swap(&mut self, a: usize, b: usize) {
        self.moves.swap(a, b);
    }

    /// Moves every capture at or after `from` ahead of every non-capture and
    /// puts those captures in [`ordering::capture_key`] order, best first.
    ///
    /// `from` is where ordering may begin, **not** where the ply began: a
    /// caller that has already placed a move at the front of its range passes
    /// the index after it, and that move is left where it is.
    ///
    /// ⚠️ **It reorders to the end of the whole buffer, not to the end of a
    /// ply**, so the caller must own everything from `from` up — which today
    /// means calling it on the ply just generated, before descending. A caller
    /// that reorders a parent's range while a child's list is live permutes
    /// the child's too, and the child walks raw indices: moves rotated behind
    /// its cursor would be searched twice and moves rotated past it skipped.
    /// The `debug_assert`s in [`NegamaxSearcher`](crate::NegamaxSearcher)
    /// compare lengths across a child and cannot see a permutation.
    ///
    /// Two moves this ordering cannot separate keep the order shunsai
    /// generated them in — the partition rotates rather than swaps, and the
    /// sort is stable.
    ///
    /// Returns where the non-captures begin, which is where
    /// [`Self::order_killers`] takes over.
    pub(crate) fn order_captures(&mut self, from: usize, position: &Position) -> usize {
        let moves = &mut self.moves[from..];
        let mut captures = 0;
        for i in 0..moves.len() {
            if ordering::capture_key(position, moves[i]).is_some() {
                moves[captures..=i].rotate_right(1);
                captures += 1;
            }
        }
        moves[..captures].sort_by(|a, b| {
            ordering::capture_key(position, *b).cmp(&ordering::capture_key(position, *a))
        });
        from + captures
    }

    /// Moves each of `killers` present at or after `from` to the front of that
    /// range, in the order given.
    ///
    /// `from` is [`Self::order_captures`]'s return value: a killer is a quiet
    /// move, so it belongs behind every capture and ahead of every other
    /// quiet.
    ///
    /// ⚠️ **A killer is played only if it is found here, and that lookup is
    /// the whole legality check.** The table is indexed by ply, so its entries
    /// were cut off in a *sibling's* position; one that is not legal in this
    /// one simply is not in the list, and nothing happens. It is the same
    /// guarantee the transposition move gets, for the same reason and by the
    /// same means.
    ///
    /// The quiet moves that stay behind keep the order shunsai generated them
    /// in, which is why this rotates rather than swaps.
    pub(crate) fn order_killers(&mut self, from: usize, killers: [Option<Move>; 2]) {
        let moves = &mut self.moves[from..];
        let mut front = 0;
        for killer in killers.into_iter().flatten() {
            if let Some(offset) = moves[front..].iter().position(|&mv| mv == killer) {
                moves[front..=front + offset].rotate_right(1);
                front += 1;
            }
        }
    }

    /// Drops everything generated at or after `base`, ending a ply.
    pub(crate) fn truncate(&mut self, base: usize) {
        self.moves.truncate(base);
    }

    /// Empties the buffer. Not `truncate(0)` in intent: a *new* search calls
    /// this, because a panic caught by `search::worker` unwinds past every
    /// `truncate` and the worker keeps the searcher alive.
    pub(crate) fn clear(&mut self) {
        self.moves.clear();
    }
}

impl Default for MoveBuf {
    /// ⚠️ Reserving, like [`MoveBuf::new`] — a default that did not would
    /// silently reintroduce the per-node allocation this type exists to avoid.
    fn default() -> Self {
        Self::new()
    }
}

/// Whether `mv` is legal in `position`, without allocating.
///
/// shunsai has no `is_legal`: generation is always fully legal, so nothing
/// inside it ever needs to ask. The caller is moves arriving over USI.
///
/// **The search is not a caller and is not going to be.** A transposition
/// move is validated by scanning the list its node generated anyway, which is
/// one pass rather than a second `generate_moves` walk — see
/// [`NegamaxSearcher`](crate::NegamaxSearcher). Anything else inside the search
/// that needs to ask "is this legal here" should reach for that shape first.
///
/// `position.legal_moves().contains(&mv)` is the obviously-correct version; it
/// allocates, and is kept as the test oracle rather than used here.
#[must_use]
pub fn is_legal(position: &Position, mv: Move) -> bool {
    position
        .generate_moves(|set| match (set, mv) {
            (
                MoveSet::Normal {
                    from,
                    promotions,
                    non_promotions,
                    ..
                },
                Move::Normal {
                    from: want_from,
                    to,
                    promote,
                },
            ) if from == want_from => {
                // A square in `promotions` alone is a compulsory promotion and
                // one in `non_promotions` alone cannot promote, so selecting by
                // the flag is exactly the legality question.
                let board = if promote { promotions } else { non_promotions };
                if board.contains(to) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            }
            (
                MoveSet::Drop { piece, to },
                Move::Drop {
                    piece: want_piece,
                    to: want_to,
                },
            ) if piece == want_piece => {
                // Both `Piece`s carry their colour, so this also rejects a drop
                // by the side not to move — the safety net under USI drop
                // notation carrying no colour of its own.
                if to.contains(want_to) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            }
            // Continue rather than fail: every set that is not the one holding
            // `mv` lands here, which on a full walk is nearly all of them —
            // usually with the variants matching and the origin guard failing.
            //
            // ⚠️ **A `MoveSet` variant added upstream also lands here, and
            // silently**, unlike `generate_captures`'s exhaustive `match` next
            // door. Legal moves of that variant would be reported illegal, and
            // `Game::push_move` is the only caller — the engine would refuse a
            // GUI's `position` line with nothing failing to compile.
            _ => ControlFlow::Continue(()),
        })
        .is_break()
}

#[cfg(test)]
mod tests {
    use shogi_core::{Color, PartialPosition, Piece, PieceKind, Square};
    use shogi_usi_parser::FromUsi;

    use super::*;

    fn position(sfen: &str) -> Position {
        Position::new(PartialPosition::from_usi(sfen).expect("test SFEN parses"))
    }

    /// `is_legal` must agree with the allocating oracle on every move the
    /// generator produces, and reject everything else — the property that
    /// makes it safe to use instead of `legal_moves().contains()`.
    fn agrees_with_oracle(position: &Position) {
        let legal = position.legal_moves();
        for mv in &legal {
            assert!(is_legal(position, *mv), "rejected a generated move {mv:?}");
        }

        // Every from-to pair on the board, promoting and not, plus every drop
        // of every hand piece to every square: a superset of the legal set, so
        // any move `is_legal` accepts outside `legal` is a false positive.
        for from in Square::all() {
            for to in Square::all() {
                for promote in [false, true] {
                    let mv = Move::Normal { from, to, promote };
                    assert_eq!(
                        is_legal(position, mv),
                        legal.contains(&mv),
                        "disagreed with the oracle on {mv:?}"
                    );
                }
            }
        }
        for color in Color::all() {
            for piece_kind in shogi_core::Hand::all_hand_pieces() {
                for to in Square::all() {
                    let mv = Move::Drop {
                        piece: Piece::new(piece_kind, color),
                        to,
                    };
                    assert_eq!(
                        is_legal(position, mv),
                        legal.contains(&mv),
                        "disagreed with the oracle on {mv:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn agrees_with_the_oracle_at_the_initial_position() {
        agrees_with_oracle(&Position::startpos());
    }

    /// A drop-heavy middlegame: both sides hold pieces, so the drop half of the
    /// walk is actually exercised, colours included.
    #[test]
    fn agrees_with_the_oracle_in_a_drop_heavy_position() {
        agrees_with_oracle(&position(
            "sfen l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1",
        ));
    }

    /// In check, generation is restricted to evasions. Anything else must be
    /// rejected — including moves that would be legal one ply earlier.
    #[test]
    fn agrees_with_the_oracle_while_in_check() {
        let position = position("sfen 4k4/9/4+R4/9/9/9/9/9/4K4 w - 1");
        assert!(position.in_check());
        agrees_with_oracle(&position);
    }

    /// Sabotage: swap the `promotions` / `non_promotions` selection. This
    /// is not the only test that fires — the oracle tests do too, because the
    /// boards differ for any move outside the promotion zone. What this one
    /// adds is the *compulsory* case, where `non_promotions` is empty.
    #[test]
    fn a_compulsory_promotion_cannot_be_declined() {
        // A black pawn on 5b can only move to 5a, where unpromoted it would
        // have no move ever again. The white king is on 1a, not 5a, so the
        // destination is empty — a king capture is never generated, which
        // would make the fixture vacuous.
        let position = position("sfen 8k/4P4/9/9/9/9/9/9/4K4 b - 1");
        let from = Square::new(5, 2).expect("5b");
        let to = Square::new(5, 1).expect("5a");

        assert!(is_legal(
            &position,
            Move::Normal {
                from,
                to,
                promote: true
            }
        ));
        assert!(!is_legal(
            &position,
            Move::Normal {
                from,
                to,
                promote: false
            }
        ));
    }

    /// The moves in `buf[range]`, in generation order.
    fn slice(buf: &MoveBuf, range: core::ops::Range<usize>) -> Vec<Move> {
        range.map(|i| buf.get(i)).collect()
    }

    /// Multiset equality. Generation *order* is an unspecified implementation
    /// detail, so nothing here may compare two sequences that came out of
    /// different calls.
    ///
    /// ⚠️ **Multiset, not set**: containment plus a length check passes a
    /// permutation that duplicates one move and drops another, which is
    /// exactly what an off-by-one in a rotate would produce — and a dropped
    /// move is a move the node never searches.
    fn same_moves(left: &[Move], right: &[Move]) {
        let key = |mv: &Move| format!("{mv:?}");
        let mut left: Vec<String> = left.iter().map(key).collect();
        let mut right: Vec<String> = right.iter().map(key).collect();
        left.sort();
        right.sort();
        assert_eq!(left, right);
    }

    #[test]
    fn the_buffer_holds_exactly_the_legal_moves() {
        for sfen in [
            "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "sfen l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1",
            // In check: generation is restricted to evasions.
            "sfen 4k4/9/4+R4/9/9/9/9/9/4K4 w - 1",
        ] {
            let board = position(sfen);
            let mut buf = MoveBuf::new();
            let base = buf.generate(&board);
            same_moves(&slice(&buf, base..buf.len()), &board.legal_moves());
        }
    }

    /// A ply generating into the buffer must leave the ply above it alone, and
    /// truncating must give that ply back exactly what it had.
    ///
    /// ⚠️ It does *not* check that the search remembers to truncate at all.
    /// That is covered by `negamax::tests::the_move_buffer_comes_back_empty`
    /// and, before it can even get that far, by the `debug_assert_eq!`s in
    /// `NegamaxSearcher::child` and `NegamaxSearcher::qsearch`.
    #[test]
    fn a_ply_does_not_disturb_the_ply_above_it() {
        let mut buf = MoveBuf::new();
        let mut board = Position::startpos();

        let root_base = buf.generate(&board);
        assert_eq!(root_base, 0);
        let root_end = buf.len();
        let root = slice(&buf, root_base..root_end);
        assert!(!root.is_empty(), "the initial position has legal moves");

        board.do_move(root[0]);
        let child_base = buf.generate(&board);
        assert_eq!(
            child_base, root_end,
            "the child started on top of the parent"
        );
        same_moves(&slice(&buf, child_base..buf.len()), &board.legal_moves());
        assert_eq!(slice(&buf, root_base..root_end), root, "the parent moved");

        buf.truncate(child_base);
        assert_eq!(buf.len(), root_end);
        assert_eq!(slice(&buf, root_base..root_end), root);
    }

    /// The capture generator against an oracle built the obvious way — every
    /// legal move whose destination is occupied. A legal move never lands on
    /// one of our own pieces, so "occupied" and "holds an enemy piece" are the
    /// same question, and the oracle asks it without going near `player_bb`.
    fn captures_agree_with_the_oracle(board: &Position) {
        let mut buf = MoveBuf::new();
        let base = buf.generate_captures(board);
        let oracle: Vec<Move> = board
            .legal_moves()
            .into_iter()
            .filter(|mv| board.piece_at(mv.to()).is_some())
            .collect();
        same_moves(&slice(&buf, base..buf.len()), &oracle);
    }

    /// Sabotage: intersect with `player_bb(side_to_move())` instead of its
    /// flip and **zero** captures are generated (`left: 0, right: 3`) — a legal
    /// move never lands on our own piece — so quiescence stands pat everywhere.
    /// Drop the `promotions` board and every capture-promotion silently
    /// disappears. Let `MoveSet::Drop` through and drops appear as captures.
    #[test]
    fn the_capture_filter_is_exactly_the_captures() {
        for sfen in [
            "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "sfen l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1",
            // In check: generation is restricted to evasions, so the captures
            // are the capturing evasions and nothing else.
            "sfen 4k4/9/4+R4/9/9/9/9/9/4K4 w - 1",
            // A capture into the promotion zone, where promoting is optional.
            "sfen 4k4/9/6p2/6S2/9/9/9/9/4K4 b - 1",
            // Two lone kings: no capture reachable, so the answer is empty.
            "sfen 4k4/9/9/9/9/9/9/9/4K4 b - 1",
        ] {
            captures_agree_with_the_oracle(&position(sfen));
        }
    }

    /// Where promotion is optional, both forms are captures and both are
    /// generated. `promotions` and `non_promotions` overlap on exactly these
    /// squares, so pushing only one board loses half of them.
    #[test]
    fn an_optional_promotion_capture_is_generated_both_ways() {
        // A black silver on 3d taking on 3c enters the promotion zone, where a
        // silver may decline — the one minor piece for which declining is a
        // real choice rather than a mistake.
        let board = position("sfen 4k4/9/6p2/6S2/9/9/9/9/4K4 b - 1");
        let mut buf = MoveBuf::new();
        let base = buf.generate_captures(&board);
        let generated = slice(&buf, base..buf.len());

        let from = Square::new(3, 4).expect("3d");
        let to = Square::new(3, 3).expect("3c");
        for promote in [false, true] {
            let mv = Move::Normal { from, to, promote };
            assert!(generated.contains(&mv), "{mv:?} was not generated");
        }
        assert_eq!(generated.len(), 2, "{generated:?}");
    }

    /// Where promotion is compulsory the square is in `promotions` alone, so
    /// the declined form must not appear — it is not a legal move.
    #[test]
    fn a_compulsory_promotion_capture_is_generated_once_promoting() {
        // A black pawn on 3b takes on 3a. An unpromoted pawn on rank 1 has no
        // move for the rest of the game, so promotion is forced.
        let board = position("sfen 6p1k/6P2/9/9/9/9/9/9/4K4 b - 1");
        let mut buf = MoveBuf::new();
        let base = buf.generate_captures(&board);
        let generated = slice(&buf, base..buf.len());

        let from = Square::new(3, 2).expect("3b");
        let to = Square::new(3, 1).expect("3a");
        assert!(generated.contains(&Move::Normal {
            from,
            to,
            promote: true
        }));
        assert!(!generated.contains(&Move::Normal {
            from,
            to,
            promote: false
        }));
        captures_agree_with_the_oracle(&board);
    }

    /// The base/truncate contract again, for the generator quiescence uses.
    /// Both generators share one buffer within a single search — an interior
    /// node calls one and its quiescence children call the other — so the
    /// property has to hold across the pair, not just within each.
    #[test]
    fn a_capture_generation_does_not_disturb_the_ply_above_it() {
        let mut buf = MoveBuf::new();
        let mut board =
            position("sfen l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1");

        let parent_base = buf.generate(&board);
        let parent_end = buf.len();
        let parent = slice(&buf, parent_base..parent_end);

        board.do_move(parent[0]);
        let child_base = buf.generate_captures(&board);
        assert_eq!(
            child_base, parent_end,
            "the child started on top of the parent"
        );
        assert_eq!(
            slice(&buf, parent_base..parent_end),
            parent,
            "the parent moved"
        );

        buf.truncate(child_base);
        assert_eq!(buf.len(), parent_end);
        assert_eq!(slice(&buf, parent_base..parent_end), parent);
    }

    /// The board every ordering test below opens from — three black silvers
    /// under four white pieces of four different values, constructed for these
    /// tests.
    ///
    /// **Three properties are what make those tests able to fail**, and this
    /// asserts each rather than assuming it: captures the ordering can rank
    /// against each other, moves that take nothing for the captures to be
    /// ranked ahead of, and a generated capture order that is **not already
    /// best-first** — on a board where it is, a missing sort goes unnoticed.
    fn ordering_fixture() -> Position {
        let board = position("sfen 4k4/9/9/1p1r1b1s1/2S1S1S2/9/9/9/4K4 b - 1");
        let mut buf = MoveBuf::new();
        let base = buf.generate(&board);
        let generated = slice(&buf, base..buf.len());
        let keys: Vec<_> = generated
            .iter()
            .filter_map(|mv| ordering::capture_key(&board, *mv))
            .collect();

        assert!(
            keys.iter().any(|key| *key != keys[0]),
            "the fixture holds no two captures this ordering can separate"
        );
        assert!(
            generated.len() - keys.len() >= 2,
            "the fixture holds fewer than two moves that take nothing"
        );
        assert!(
            !keys.is_sorted_by(|a, b| a >= b),
            "the fixture generates its captures best-first already: {keys:?}"
        );
        board
    }

    /// The partition: every capture ahead of everything that takes nothing,
    /// and the same moves as before.
    ///
    /// Sabotage: count the captures without moving them. It goes red on
    /// `the_captures_come_out_best_first` and on `negamax`'s
    /// `the_transposition_move_is_searched_first` too.
    #[test]
    fn ordering_puts_every_capture_ahead_of_every_quiet_move() {
        let board = ordering_fixture();
        let mut buf = MoveBuf::new();
        let base = buf.generate(&board);
        let generated = slice(&buf, base..buf.len());

        buf.order_captures(base, &board);
        let ordered = slice(&buf, base..buf.len());
        same_moves(&ordered, &generated);

        let captures = ordered.partition_point(|mv| ordering::capture_key(&board, *mv).is_some());
        for (i, mv) in ordered.iter().enumerate() {
            assert_eq!(
                ordering::capture_key(&board, *mv).is_some(),
                i < captures,
                "{mv:?} came out at {i} of {captures} captures"
            );
        }
    }

    /// The sort: what wins most is tried first.
    ///
    /// Sabotage: drop the `sort_by` and keep the partition;
    /// `negamax`'s `the_transposition_move_is_searched_first` goes red with
    /// it.
    #[test]
    fn the_captures_come_out_best_first() {
        let board = ordering_fixture();
        let mut buf = MoveBuf::new();
        let base = buf.generate(&board);
        buf.order_captures(base, &board);
        let keys: Vec<_> = (base..buf.len())
            .filter_map(|i| ordering::capture_key(&board, buf.get(i)))
            .collect();
        assert!(keys.is_sorted_by(|a, b| a >= b), "{keys:?}");
    }

    /// Ordering from `base + 1` leaves `base` alone — what keeps an interior
    /// node's transposition move in front of the captures it has just ranked.
    #[test]
    fn ordering_past_the_front_leaves_the_front_where_it_is() {
        let board = ordering_fixture();
        let mut buf = MoveBuf::new();
        let base = buf.generate(&board);
        // Parked at the front: a move that takes nothing, so no key of its own
        // could have put it there and finding it there means it was left there.
        let quiet = (base..buf.len())
            .find(|&i| ordering::capture_key(&board, buf.get(i)).is_none())
            .expect("the fixture holds a move that takes nothing");
        buf.swap(base, quiet);
        let front = buf.get(base);

        buf.order_captures(base + 1, &board);
        assert_eq!(buf.get(base), front);
        assert!(
            ordering::capture_key(&board, buf.get(base + 1)).is_some(),
            "the captures did not close up behind the front"
        );
    }

    /// Moves this ordering cannot separate come out in the order shunsai
    /// generated them.
    ///
    /// Sabotage: partition by `swap` rather than by `rotate_right` and this
    /// goes red.
    ///
    /// ⚠️ **The sort's half of the same rule has no test.** `sort_unstable_by`
    /// in place of `sort_by` left the workspace green, `bench`'s counts
    /// included.
    #[test]
    fn moves_of_equal_rank_keep_their_generated_order() {
        let board = ordering_fixture();
        let mut buf = MoveBuf::new();
        let base = buf.generate(&board);
        let generated = slice(&buf, base..buf.len());
        buf.order_captures(base, &board);
        let ordered = slice(&buf, base..buf.len());

        let generated_index = |mv: &Move| {
            generated
                .iter()
                .position(|g| g == mv)
                .expect("was generated")
        };
        for (i, left) in ordered.iter().enumerate() {
            for right in &ordered[i + 1..] {
                if ordering::capture_key(&board, *left) == ordering::capture_key(&board, *right) {
                    assert!(
                        generated_index(left) < generated_index(right),
                        "{left:?} and {right:?} rank the same and came out swapped"
                    );
                }
            }
        }
    }

    /// Two quiet moves from the board above, and the run of quiet moves they
    /// were taken from.
    ///
    /// **What makes the tests below able to fail is that neither killer is
    /// already at the front of that run**: a killer the ordering would have
    /// put first anyway is indistinguishable from one that was never looked
    /// for. They are taken from the back, and that is asserted rather than
    /// assumed.
    fn killer_fixture() -> (Position, [Option<Move>; 2], Vec<Move>) {
        let board = ordering_fixture();
        let mut buf = MoveBuf::new();
        let base = buf.generate(&board);
        let quiets_from = buf.order_captures(base, &board);
        let quiets = slice(&buf, quiets_from..buf.len());
        assert!(
            quiets.len() >= 4,
            "the fixture holds {} moves that take nothing, too few to pick two \
             from the back of",
            quiets.len()
        );
        let killers = [
            Some(quiets[quiets.len() - 1]),
            Some(quiets[quiets.len() - 2]),
        ];
        assert!(
            killers[0] != Some(quiets[0]) && killers[1] != Some(quiets[1]),
            "a killer is already where ordering would leave it: {killers:?}"
        );
        (board, killers, quiets)
    }

    /// Where a killer goes: behind every capture, ahead of every other quiet
    /// move, and in the order the table holds them.
    ///
    /// Sabotage: find the killer and leave it where it is. All three killer
    /// tests here go red.
    #[test]
    fn killers_come_out_behind_the_captures_and_ahead_of_the_other_quiets() {
        let (board, killers, _) = killer_fixture();
        let mut buf = MoveBuf::new();
        let base = buf.generate(&board);
        let generated = slice(&buf, base..buf.len());

        let quiets_from = buf.order_captures(base, &board);
        buf.order_killers(quiets_from, killers);
        same_moves(&slice(&buf, base..buf.len()), &generated);

        assert!(
            ordering::capture_key(&board, buf.get(quiets_from - 1)).is_some(),
            "the move ahead of the killers takes nothing, so they displaced a capture"
        );
        assert_eq!(buf.get(quiets_from), killers[0].expect("picked above"));
        assert_eq!(buf.get(quiets_from + 1), killers[1].expect("picked above"));
    }

    /// A killer cut a *sibling* position off, so this one need not be able to
    /// play it. Finding it in the list is the whole legality check, and a
    /// killer that is not there changes nothing — including for the killer
    /// beside it, which still lands at the front.
    ///
    /// Sabotage: advance the front whether or not the killer was found. This
    /// goes red, and so do eight tests in `negamax`.
    #[test]
    fn a_killer_this_node_cannot_play_moves_nothing() {
        let (board, killers, _) = killer_fixture();
        // Black holds no pieces on this board, so no drop of any kind is in
        // the list — a move that is legal in other positions and not here.
        let absent = Move::Drop {
            piece: Piece::new(PieceKind::Pawn, Color::Black),
            to: Square::SQ_5F,
        };
        let mut buf = MoveBuf::new();
        let base = buf.generate(&board);
        assert!(
            !slice(&buf, base..buf.len()).contains(&absent),
            "the fixture can play {absent:?} after all"
        );

        let quiets_from = buf.order_captures(base, &board);
        let before = slice(&buf, base..buf.len());
        buf.order_killers(quiets_from, [Some(absent), None]);
        assert_eq!(slice(&buf, base..buf.len()), before);

        buf.order_killers(quiets_from, [Some(absent), killers[0]]);
        assert_eq!(buf.get(quiets_from), killers[0].expect("picked above"));
    }

    /// The quiet moves a killer passes keep the order shunsai generated them
    /// in — the same rule the capture partition follows, and for the same
    /// reason.
    ///
    /// Sabotage: `swap` instead of `rotate_right`. This goes red and the other
    /// two killer tests do not.
    #[test]
    fn the_quiet_moves_a_killer_passes_keep_their_generated_order() {
        let (board, killers, quiets) = killer_fixture();
        let mut buf = MoveBuf::new();
        let base = buf.generate(&board);
        let quiets_from = buf.order_captures(base, &board);
        buf.order_killers(quiets_from, killers);

        let rest: Vec<Move> = slice(&buf, quiets_from + 2..buf.len());
        let expected: Vec<Move> = quiets
            .iter()
            .copied()
            .filter(|mv| !killers.contains(&Some(*mv)))
            .collect();
        assert_eq!(rest, expected);
    }

    /// One allocation, sized for the deepest search, taken before the search
    /// starts. Measured in elements: `Move`'s size is not a guarantee Rust
    /// makes, so a byte figure here would be pinning something nobody promised.
    #[test]
    fn the_buffer_reserves_a_whole_search_up_front() {
        let buf = MoveBuf::new();
        assert!(buf.moves.capacity() >= MAX_LEGAL_MOVES * MAX_PLY);
        assert_eq!(MoveBuf::default().moves.capacity(), buf.moves.capacity());
    }

    /// Sabotage: drop the colour comparison in the drop arm and this passes a
    /// wrong-colour drop. It is what turns USI's colourless drop notation from
    /// a silent board corruption into a rejected command.
    #[test]
    fn a_drop_by_the_wrong_colour_is_rejected() {
        let position = position("sfen 4k4/9/9/9/9/9/9/9/4K4 b Pp 1");
        let to = Square::new(5, 5).expect("5e");

        assert!(is_legal(
            &position,
            Move::Drop {
                piece: Piece::new(PieceKind::Pawn, Color::Black),
                to
            }
        ));
        assert!(!is_legal(
            &position,
            Move::Drop {
                piece: Piece::new(PieceKind::Pawn, Color::White),
                to
            }
        ));
    }
}
