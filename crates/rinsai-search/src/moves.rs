//! Move helpers that shunsai deliberately does not provide.

use core::ops::ControlFlow;

use shogi_core::Move;
use shunsai::{MoveSet, Position};

use crate::score::MAX_PLY;

/// The most legal moves any shogi position has.
///
/// It is the size a move buffer has to be, and shunsai's own benches use the
/// same number (`benches/suite/common.rs`).
pub const MAX_LEGAL_MOVES: usize = 593;

/// One move list, shared by every ply of one search.
///
/// [`Position::generate_moves`] takes `&self`, so nothing can play a move from
/// inside its callback: anything recursive has to collect first, and this is
/// where the collection goes. The shape — one allocation of
/// `MAX_LEGAL_MOVES * MAX_PLY`, sliced per ply, generation appending and the
/// caller truncating back on the way out — is shunsai's own
/// `perft_materialize` (`examples/perft.rs`), and the alternatives it was
/// chosen over are argued in PROGRESS.md.
///
/// **The moves come out by value, one at a time, and that is load-bearing.**
/// A `&[Move]` handed back here would borrow the buffer for as long as the
/// caller iterates, and the caller wants to recurse through `&mut self` in the
/// middle of that loop. Indexing copies a `Move` — small and [`Copy`] — and
/// ends the borrow on the same line.
///
/// E1 makes the element scored. That lands as a parallel array filled by
/// [`Self::generate`] plus a selection step, and [`Self::get`] keeps its
/// signature, so the search's loop does not change shape.
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
    /// This is quiescence search's generator. shunsai has no captures-only
    /// generation — staged generation is E1, DESIGN.md §6 — so the full legal
    /// walk still runs. What does **not** run is materialisation: a [`MoveSet`]
    /// hands over its destinations as [`Bitboard`](shunsai::Bitboard)s, so this
    /// intersects two boards per set and builds a [`Move`] only for the
    /// captures. [`MoveSet::Drop`] is skipped whole, because a drop can never
    /// capture — and drops are most of what a shogi move list is. What that
    /// leaves on the table is measured in PROGRESS.md, and it is the size of
    /// the prize DESIGN.md §6 assigns to E1's staged generation.
    ///
    /// The result is a subset of [`Self::generate`]'s, so one ply still cannot
    /// exceed [`MAX_LEGAL_MOVES`].
    pub(crate) fn generate_captures(&mut self, position: &Position) -> usize {
        let base = self.moves.len();
        // Read before any move is made. Reading the captured piece *after*
        // `do_move` — or intersecting with our own board — makes every move
        // look like a capture, and quiescence would then never terminate.
        let them = position.player_bb(position.side_to_move().flip());
        let moves = &mut self.moves;
        let _ = position.generate_moves(|set| {
            // Exhaustive rather than `if let`, and the discarded arm is spelled
            // out. A `MoveSet` variant added upstream would otherwise be
            // silently missing captures — a strength bug with no symptom — where
            // this makes it a compile error naming the place to decide. Same
            // argument as `usi::options::Slot`, where it caught a real one at
            // step 1; a new variant is a semver-major bump and so would be
            // noticed anyway, but being told exactly where costs nothing.
            match set {
                MoveSet::Normal {
                    from,
                    promotions,
                    non_promotions,
                    ..
                } => {
                    // The two boards overlap where promotion is optional, so a
                    // capture that may promote is emitted both ways: both are
                    // legal and both are captures. Where promotion is
                    // compulsory the square is in `promotions` alone, so only
                    // the promoting form is emitted. Choosing between the two
                    // is ordering, which is E1's.
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
                // A drop can never capture: shunsai masks drop targets with the
                // empty squares, so the whole fan-out is discarded here rather
                // than filtered move by move. That is also most of what a shogi
                // move list is, which is why this is where the cost goes.
                MoveSet::Drop { .. } => {}
            }
            ControlFlow::Continue(())
        });
        self.debug_assert_reserved();
        base
    }

    /// Fails if the buffer has outgrown the reservation [`Self::new`] made.
    ///
    /// The reservation covers one full move list at every ply, so exceeding it
    /// means a ply generated twice or the search ran past [`MAX_PLY`]. Either
    /// way the symptom in release is a reallocation on the hot path rather than
    /// a wrong answer, which is exactly the kind of thing that goes unnoticed.
    fn debug_assert_reserved(&self) {
        debug_assert!(
            self.moves.len() <= MAX_LEGAL_MOVES * MAX_PLY,
            "the move buffer outgrew its reservation at {} moves",
            self.moves.len()
        );
    }

    /// One past the last move generated.
    pub(crate) fn len(&self) -> usize {
        self.moves.len()
    }

    /// The move at `index`, by value.
    pub(crate) fn get(&self, index: usize) -> Move {
        self.moves[index]
    }

    /// Drops everything generated at or after `base`, ending a ply.
    pub(crate) fn truncate(&mut self, base: usize) {
        self.moves.truncate(base);
    }

    /// Empties the buffer.
    ///
    /// Not the same as `truncate(0)` in intent: this is what a *new* search
    /// calls, because a panic caught by `search::worker` unwinds past every
    /// `truncate` and leaves the searcher — which the worker keeps alive —
    /// holding a previous search's moves.
    pub(crate) fn clear(&mut self) {
        self.moves.clear();
    }
}

impl Default for MoveBuf {
    /// Reserving, like [`MoveBuf::new`]. A default that did not reserve would
    /// silently reintroduce the per-node allocation the type exists to avoid.
    fn default() -> Self {
        Self::new()
    }
}

/// Whether `mv` is legal in `position`, without allocating.
///
/// shunsai has no `is_legal`: generation is always fully legal, so nothing
/// inside it ever needs to ask. A search engine does. Two callers exist —
/// moves arriving over USI (and, from E2, CSA), and from E0 step 3b a
/// transposition-table move before it is played, since a 64-bit key collision
/// would otherwise feed [`Position::do_move`] a move from a different position
/// and trip its `expect`s.
///
/// The obviously-correct version is `position.legal_moves().contains(&mv)`;
/// that allocates, and is kept as the test oracle rather than used here.
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
                // The two boards overlap where promotion is optional: a square
                // in `promotions` alone is a compulsory promotion and one in
                // `non_promotions` alone cannot promote, so selecting by the
                // flag is exactly the legality question.
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
                // by the side that is not to move — which is the safety net
                // under USI drop notation carrying no colour of its own.
                if to.contains(want_to) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            }
            // Continue rather than reporting failure, so the walk stays correct
            // even if shunsai ever emits two `MoveSet`s sharing a `from`.
            // Nothing in shunsai's public documentation says how moves are
            // grouped into sets — one per origin is what it does today, but it
            // does not promise that — so this makes no assumption either way.
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

    /// Sabotage: swap the `promotions` / `non_promotions` selection and this is
    /// the assertion that fires — the two boards overlap almost everywhere, so
    /// only a compulsory promotion tells them apart.
    #[test]
    fn a_compulsory_promotion_cannot_be_declined() {
        // A black pawn on 5b can only move to 5a, where an unpromoted pawn
        // would have no move for the rest of the game. The white king is on 1a
        // rather than 5a so that the pawn's one destination is empty — a king
        // capture is never generated, which would make the fixture vacuous.
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

    /// Set equality. Generation *order* is an unspecified implementation
    /// detail, so nothing here may compare two sequences that came out of
    /// different calls.
    fn same_moves(left: &[Move], right: &[Move]) {
        assert_eq!(left.len(), right.len(), "different numbers of moves");
        for mv in left {
            assert!(right.contains(mv), "{mv:?} is missing from the other side");
        }
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
    /// This is the property that lets one allocation serve the whole search.
    /// Note what it does *not* check: that the search remembers to truncate at
    /// all. Forgetting is a leak rather than a wrong answer — the base is read
    /// before each generation, so every ply still sees only its own moves — so
    /// it needs a test on the search itself, and has one
    /// (`negamax::tests::the_move_buffer_comes_back_empty`).
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
    /// flip, and every quiet move reads as a capture — quiescence then never
    /// terminates. Drop the `promotions` board and every capture-promotion
    /// silently disappears from quiescence, which no score assertion would
    /// notice. Let `MoveSet::Drop` through and drops appear as captures.
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
            // Two lone kings: no capture is reachable, so the answer is empty.
            // The case a filter that returns everything would fail loudest on.
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
        // A black silver on 3d taking the white pawn on 3c enters the promotion
        // zone, where a silver may decline — the one minor piece for which
        // declining is a real choice rather than a mistake.
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
