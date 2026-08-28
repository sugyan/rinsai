//! Move ordering: which of a node's moves is tried before the others.

use shogi_core::{Color, Move, PieceKind, Square};
use shunsai::Position;

use crate::eval;
use crate::score::Depth;

/// How promising `mv` is as a capture in `position`, larger first, or `None`
/// when it takes nothing.
///
/// Two `piece_at` reads and [`key_of`]; the ranking itself is that function's,
/// so that a test can ask about a (victim, attacker, promote) triple without
/// needing a board that holds it.
pub(crate) fn capture_key(position: &Position, mv: Move) -> Option<(i32, i32)> {
    // First, so that a drop — which lands on an empty square and can therefore
    // never capture — leaves here rather than through the `from` lookup below.
    let (victim, _) = position.piece_at(mv.to())?.to_parts();
    let (attacker, _) = position.piece_at(mv.from()?)?.to_parts();
    Some(key_of(victim, attacker, mv.is_promoting()))
}

/// The rank of one capture, larger first.
///
/// The pair is a strict priority, and the type is what enforces it: `(i32,
/// i32)` compares its first element first, so no attacker value can reverse a
/// difference in material.
///
/// * **What the move wins** — the victim's [`eval::capture_gain`] plus what
///   promoting adds.
/// * **What wins it**, only where the first ties: the cheapest attacker.
///   ⚠️ **The king is the cheapest of all, and that is deliberate.** Every move
///   here came out of shunsai's fully legal generation, so a king capture that
///   was generated leaves the king unattacked — the victim was undefended and
///   the material is won outright. `board_value` prices the king at zero for a
///   reason of its own, and this is the one reading of that zero that happens
///   to be right; `the_king_takes_first_because_a_legal_king_capture_is_free`
///   is what says so if either side of it changes.
pub(crate) fn key_of(victim: PieceKind, attacker: PieceKind, promote: bool) -> (i32, i32) {
    let promotion = if promote { promotion_gain(attacker) } else { 0 };
    (
        eval::capture_gain(victim) + promotion,
        -eval::board_value(attacker),
    )
}

/// What promoting a `kind` adds to the mover's material. Zero for a kind that
/// cannot promote.
fn promotion_gain(kind: PieceKind) -> i32 {
    kind.promote().map_or(0, |promoted| {
        eval::board_value(promoted) - eval::board_value(kind)
    })
}

/// How readily a quiet move has cut a node off, larger first.
///
/// **Indexed by (side, piece kind, destination)**, which is what lets one
/// table cover a whole shogi move list: a drop has no origin, so the `(from,
/// to)` board a chess engine indexes this by cannot represent half of the
/// quiet moves here at all.
///
/// ⚠️ **Promotion is not part of the index**, so a quiet promotion and the
/// same move declining it share one entry — と金作り and the pawn push under
/// it are one number. `a_promotion_and_its_declining_twin_share_an_entry` is
/// what says so the day the index is widened.
#[derive(Debug)]
pub(crate) struct HistoryTable {
    /// One entry per (side, piece kind, destination). Its length never
    /// varies, so [`Self::clear`] refills rather than resizes.
    values: Vec<i32>,
}

impl HistoryTable {
    /// What [`Self::record`]'s decay holds every entry at or below, so that
    /// nothing has to sweep the table.
    ///
    /// ⚠️ **It is the decay's fixed point for every bonus, so it is also
    /// where every recorded entry ends up.** Depth sets how fast an entry
    /// climbs, not where it stops, and with no malus nothing brings one back
    /// down — a long enough search ranks its saturated entries equal.
    /// `a_deeper_cutoff_is_worth_more_until_both_saturate` carries both
    /// halves.
    ///
    /// ⚠️ **A bonus reaching it would break that**, which is why `record`
    /// asserts the caller's depth stays under.
    pub(crate) const CEILING: i32 = 1 << 14;

    pub(crate) fn new() -> Self {
        Self {
            values: vec![0; Color::NUM * PieceKind::NUM * Square::NUM],
        }
    }

    /// Forgets everything. A new search is a new table.
    pub(crate) fn clear(&mut self) {
        self.values.fill(0);
    }

    /// How readily `mv` has cut a node off in this search, `0` for a move
    /// nothing has been recorded about.
    pub(crate) fn value(&self, position: &Position, mv: Move) -> i32 {
        Self::index(position, mv).map_or(0, |index| self.values[index])
    }

    /// Records that `mv` cut a node off at `depth`.
    ///
    /// The caller owes two things: `mv` takes nothing — a capture is ordered
    /// by [`capture_key`] and would never be looked up here — and `position`
    /// is the one `mv` is played *from*, because the index reads the piece on
    /// its origin.
    ///
    /// The bonus grows with the depth the cutoff was proved at, and the term
    /// subtracted with it is what stops an entry running away: it is nothing
    /// at zero and the whole bonus at [`Self::CEILING`], so every entry
    /// approaches the ceiling and none passes it.
    pub(crate) fn record(&mut self, position: &Position, mv: Move, depth: Depth) {
        debug_assert!(depth > 0, "a cutoff at depth {depth}");
        debug_assert!(
            capture_key(position, mv).is_none(),
            "a capture was recorded as history: {mv:?}"
        );
        let bonus = depth * depth;
        debug_assert!(
            bonus < Self::CEILING,
            "a bonus of {bonus} at depth {depth} is not under the ceiling"
        );
        let Some(index) = Self::index(position, mv) else {
            return;
        };
        let value = &mut self.values[index];
        *value += bonus - *value * bonus / Self::CEILING;
    }

    /// Where `mv` sits in [`Self::values`], or `None` when its origin square
    /// holds nothing — a move that cannot be played from `position` at all.
    fn index(position: &Position, mv: Move) -> Option<usize> {
        let (kind, color) = match mv {
            // Before any promotion: the promoted form is what lands, and the
            // ⚠️ on the type says what sharing an entry with it costs.
            Move::Normal { from, .. } => position.piece_at(from)?.to_parts(),
            Move::Drop { piece, .. } => piece.to_parts(),
        };
        let piece = color.array_index() * PieceKind::NUM + kind.array_index();
        Some(piece * Square::NUM + mv.to().array_index())
    }
}

#[cfg(test)]
mod tests {
    use shogi_core::{PartialPosition, Piece};
    use shogi_usi_parser::FromUsi;

    use super::*;

    fn position(sfen: &str) -> Position {
        Position::new(PartialPosition::from_usi(sfen).expect("test SFEN parses"))
    }

    /// The dearer victim first, whatever takes it.
    ///
    /// Sabotage: swap the pair's two elements.
    #[test]
    fn the_dearer_victim_ranks_first() {
        assert!(
            key_of(PieceKind::Rook, PieceKind::Pawn, false)
                > key_of(PieceKind::Pawn, PieceKind::Rook, false)
        );
        assert!(
            key_of(PieceKind::Gold, PieceKind::Gold, false)
                > key_of(PieceKind::Silver, PieceKind::Pawn, false)
        );
    }

    /// Equal victims, cheapest attacker first — the only thing the second
    /// element of the pair ever decides.
    ///
    /// Sabotage: drop the negation on `board_value`.
    #[test]
    fn an_equal_victim_is_taken_with_the_cheaper_piece() {
        assert!(
            key_of(PieceKind::Gold, PieceKind::Pawn, false)
                > key_of(PieceKind::Gold, PieceKind::Rook, false)
        );
    }

    /// ⚠️ **The king outranks every other attacker**, because a king capture
    /// that shunsai generated is a capture of an undefended piece.
    ///
    /// Sabotage: price the king above the other attackers in `eval`'s table —
    /// the reading of `BOARD[King] == 0` this rests on — and it goes red.
    #[test]
    fn the_king_takes_first_because_a_legal_king_capture_is_free() {
        for attacker in PieceKind::all() {
            if attacker == PieceKind::King {
                continue;
            }
            assert!(
                key_of(PieceKind::Gold, PieceKind::King, false)
                    > key_of(PieceKind::Gold, attacker, false),
                "{attacker:?} ranks at or above the king"
            );
        }
    }

    /// Promoting is a material event of its own, so the promoting form of a
    /// capture ranks above the same capture declining it.
    ///
    /// Sabotage: drop the `promotion` term from `key_of`.
    #[test]
    fn a_capture_that_promotes_ranks_above_the_same_capture_declining() {
        for attacker in [
            PieceKind::Pawn,
            PieceKind::Lance,
            PieceKind::Knight,
            PieceKind::Silver,
            PieceKind::Bishop,
            PieceKind::Rook,
        ] {
            assert!(
                key_of(PieceKind::Pawn, attacker, true) > key_of(PieceKind::Pawn, attacker, false),
                "{attacker:?}"
            );
        }
    }

    /// A promoted victim is won at its board value and its **unpromoted** hand
    /// value: a と金 goes to the hand as a pawn, so taking one wins less than
    /// taking the gold it moves like.
    ///
    /// Sabotage, both directions: drop `capture_gain`'s hand term, or take the
    /// hand value without unpromoting first.
    #[test]
    fn a_promoted_victim_is_won_at_its_board_value_and_an_unpromoted_hand() {
        assert!(
            key_of(PieceKind::ProRook, PieceKind::Pawn, false)
                > key_of(PieceKind::Rook, PieceKind::Pawn, false)
        );
        assert!(
            key_of(PieceKind::ProPawn, PieceKind::Pawn, false)
                < key_of(PieceKind::Gold, PieceKind::Pawn, false)
        );
    }

    /// A move that takes nothing has no key at all — the partition
    /// [`MoveBuf::order_captures`](crate::moves::MoveBuf::order_captures) runs
    /// is this answer and no second question.
    #[test]
    fn a_move_that_takes_nothing_has_no_key() {
        let board = position("sfen 4k4/9/9/9/9/9/9/9/4K4 b P 1");
        let quiet = Move::Normal {
            from: Square::new(5, 9).expect("5i"),
            to: Square::new(5, 8).expect("5h"),
            promote: false,
        };
        assert_eq!(capture_key(&board, quiet), None);

        let drop = Move::Drop {
            piece: Piece::new(PieceKind::Pawn, Color::Black),
            to: Square::new(5, 5).expect("5e"),
        };
        assert_eq!(capture_key(&board, drop), None);
    }

    /// The key read off a real board is [`key_of`]'s, for the victim on the
    /// destination and the attacker on the origin.
    ///
    /// Sabotage: read the attacker from `mv.to()` rather than `mv.from()`, or
    /// pass a constant in place of `mv.is_promoting()`.
    #[test]
    fn the_board_lookups_find_the_victim_and_the_attacker() {
        // A black rook on 5f under a white silver on 5e, and a black pawn on
        // 3d under a white pawn on 3c where promotion is optional.
        let board = position("sfen 4k4/9/6p2/6P2/4s4/4R4/9/9/4K4 b - 1");

        let take_with_rook = Move::Normal {
            from: Square::new(5, 6).expect("5f"),
            to: Square::new(5, 5).expect("5e"),
            promote: false,
        };
        assert_eq!(
            capture_key(&board, take_with_rook),
            Some(key_of(PieceKind::Silver, PieceKind::Rook, false))
        );

        let take_with_pawn = Move::Normal {
            from: Square::new(3, 4).expect("3d"),
            to: Square::new(3, 3).expect("3c"),
            promote: true,
        };
        assert_eq!(
            capture_key(&board, take_with_pawn),
            Some(key_of(PieceKind::Pawn, PieceKind::Pawn, true))
        );
    }

    /// The king case on a real board, where it is reachable: in check, with a
    /// cheaper attacker of the same piece available.
    ///
    /// **The fixture is what makes this able to fail** — a gold on 5e giving
    /// check, takeable by the king on 5f and by the silver on 4f, and
    /// undefended, so both captures are generated.
    #[test]
    fn a_king_capture_and_a_cheaper_one_rank_the_king_first() {
        let board = position("sfen 8k/9/9/9/4g4/4KS3/9/9/9 b - 1");
        assert!(board.in_check(), "the fixture stopped being a check");
        let to = Square::new(5, 5).expect("5e");
        let by = |file, rank| {
            capture_key(
                &board,
                Move::Normal {
                    from: Square::new(file, rank).expect("a square"),
                    to,
                    promote: false,
                },
            )
            .expect("a capture")
        };
        assert!(by(5, 6) > by(4, 6), "the silver outranked the king");
    }

    /// The board the history tests index into: a black pawn on 5g and a black
    /// silver on 6g, both of which reach 5f, under a white pawn on 5e that
    /// reaches it too.
    ///
    /// **What makes the separation test able to fail is that the three origins
    /// hold three different (side, kind) pieces** — asserted here rather than
    /// read off the diagram, so a fixture edited later says so.
    fn history_fixture() -> Position {
        let board = position("sfen 4k4/9/9/9/4p4/9/3SP4/9/4K4 b - 1");
        for (file, rank, kind, color) in [
            (5, 7, PieceKind::Pawn, Color::Black),
            (6, 7, PieceKind::Silver, Color::Black),
            (5, 5, PieceKind::Pawn, Color::White),
        ] {
            let square = Square::new(file, rank).expect("a square");
            assert_eq!(
                board.piece_at(square).map(Piece::to_parts),
                Some((kind, color)),
                "the fixture moved its {color:?} {kind:?}"
            );
        }
        board
    }

    /// A board move from `(file, rank)` to `(file, rank)`.
    fn normal(from: (u8, u8), to: (u8, u8), promote: bool) -> Move {
        Move::Normal {
            from: Square::new(from.0, from.1).expect("a square"),
            to: Square::new(to.0, to.1).expect("a square"),
            promote,
        }
    }

    /// A table nothing has been recorded into ranks every move the same, so
    /// the ordering it feeds is the generated one until a cutoff says
    /// otherwise.
    #[test]
    fn a_fresh_table_answers_zero_for_every_move() {
        let board = history_fixture();
        let history = HistoryTable::new();
        for mv in &board.legal_moves() {
            assert_eq!(history.value(&board, *mv), 0, "{mv:?}");
        }
    }

    /// The whole point: a move that cut a node off outranks one that never
    /// has.
    ///
    /// Sabotage: return from `record` before it writes.
    #[test]
    fn a_recorded_move_outranks_one_that_was_not() {
        let board = history_fixture();
        let mut history = HistoryTable::new();
        let recorded = normal((5, 7), (5, 6), false);
        let other = normal((6, 7), (6, 6), false);
        history.record(&board, recorded, 4);
        assert!(history.value(&board, recorded) > history.value(&board, other));
    }

    /// ⚠️ **Three dimensions, and dropping any one of them collides.** The
    /// black pawn recorded here shares its destination with a white pawn and
    /// with a black silver, so side and kind each separate a pair nothing else
    /// does; the silver then separates two destinations.
    ///
    /// Sabotage: drop the `color.array_index()` term, the `kind.array_index()`
    /// term or the `mv.to()` term from `index`.
    #[test]
    fn the_index_separates_the_side_the_kind_and_the_destination() {
        let board = history_fixture();
        let mut history = HistoryTable::new();

        let black_pawn = normal((5, 7), (5, 6), false);
        history.record(&board, black_pawn, 4);
        assert!(history.value(&board, black_pawn) > 0);
        assert_eq!(
            history.value(&board, normal((5, 5), (5, 6), false)),
            0,
            "the white pawn to the same square"
        );
        assert_eq!(
            history.value(&board, normal((6, 7), (5, 6), false)),
            0,
            "the black silver to the same square"
        );

        let silver = normal((6, 7), (5, 6), false);
        history.record(&board, silver, 4);
        assert!(history.value(&board, silver) > 0);
        assert_eq!(
            history.value(&board, normal((6, 7), (7, 6), false)),
            0,
            "the same silver to another square"
        );
    }

    /// The piece a drop puts down is the only thing separating two drops to
    /// one square, and it comes out of the move rather than off the board.
    ///
    /// Sabotage: read the drop's kind off the board rather than out of the
    /// move, and both drops answer for the empty destination.
    #[test]
    fn a_drop_is_indexed_by_the_piece_it_puts_down() {
        let board = position("sfen 4k4/9/9/9/9/9/9/9/4K4 b SP 1");
        let to = Square::new(5, 5).expect("5e");
        let drop = |kind| Move::Drop {
            piece: Piece::new(kind, Color::Black),
            to,
        };
        let mut history = HistoryTable::new();
        history.record(&board, drop(PieceKind::Pawn), 4);
        assert!(history.value(&board, drop(PieceKind::Pawn)) > 0);
        assert_eq!(history.value(&board, drop(PieceKind::Silver)), 0);
    }

    /// ⚠️ **A quiet promotion and the same move declining it share one
    /// entry**, because promotion is not in the index — と金作り and the pawn
    /// push under it are one number. This is here so that widening the index
    /// reddens something instead of passing silently.
    #[test]
    fn a_promotion_and_its_declining_twin_share_an_entry() {
        // A black pawn on 5d stepping to 5c, inside the promotion zone and
        // with a move left if it declines, so the board offers both forms.
        let board = position("sfen 4k4/9/9/4P4/9/9/9/9/4K4 b - 1");
        let promoting = normal((5, 4), (5, 3), true);
        let declining = normal((5, 4), (5, 3), false);
        let legal = board.legal_moves();
        assert!(
            legal.contains(&promoting) && legal.contains(&declining),
            "the fixture stopped offering both forms"
        );

        let mut history = HistoryTable::new();
        history.record(&board, promoting, 4);
        assert_eq!(
            history.value(&board, declining),
            history.value(&board, promoting)
        );
    }

    /// The decay's two jobs: no entry passes the ceiling, and the arithmetic
    /// getting there stays inside an `i32`.
    ///
    /// **The depths are what make this able to fail** — the shallowest is the
    /// slowest climb the table has and the deepest is the largest product
    /// `record` can form, and both have to land on the ceiling without
    /// passing it. ⚠️ **The overflow half is checked by running in debug**,
    /// where an `i32` overflow panics.
    ///
    /// Sabotage: drop the subtracted term from `record`.
    #[test]
    fn the_decay_holds_the_ceiling() {
        let board = history_fixture();
        let recorded = normal((5, 7), (5, 6), false);
        for depth in [1, 32, 127] {
            let mut history = HistoryTable::new();
            for _ in 0..HistoryTable::CEILING {
                history.record(&board, recorded, depth);
                assert!(history.value(&board, recorded) <= HistoryTable::CEILING);
            }
            assert_eq!(
                history.value(&board, recorded),
                HistoryTable::CEILING,
                "depth {depth} never reached the ceiling, so it bounded nothing"
            );
        }
    }

    /// A cutoff proved deeper is worth more **while the entry is still
    /// climbing**, and worth exactly the same once it has arrived.
    ///
    /// ⚠️ **Depth sets the rate, not the destination.** Bonus-only recording
    /// has nothing to pull an entry back down, so two entries fed different
    /// depths converge on [`HistoryTable::CEILING`] and stop being ordered by
    /// depth at all. The second half is asserted so that the day a malus
    /// arrives, it says so.
    ///
    /// Sabotage: replace `depth * depth` with a constant and the first half
    /// goes red; the second stays green, which is the point of it.
    #[test]
    fn a_deeper_cutoff_is_worth_more_until_both_saturate() {
        let board = history_fixture();
        let recorded = normal((5, 7), (5, 6), false);
        let after = |depth, records| {
            let mut history = HistoryTable::new();
            for _ in 0..records {
                history.record(&board, recorded, depth);
            }
            history.value(&board, recorded)
        };
        assert!(after(4, 1) > after(2, 1), "one record apiece");
        assert_eq!(
            after(4, HistoryTable::CEILING),
            after(2, HistoryTable::CEILING),
            "saturated entries still tell the two depths apart"
        );
    }

    /// A move out of an empty square has no entry, the way a move that takes
    /// nothing has no capture key. Nothing in the search produces one; the
    /// answer is a number rather than a panic so the lookup is total.
    #[test]
    fn a_move_out_of_an_empty_square_has_no_entry() {
        let board = history_fixture();
        let empty = normal((1, 1), (1, 2), false);
        assert!(
            board.piece_at(Square::new(1, 1).expect("1a")).is_none(),
            "the fixture put a piece on 1a"
        );
        let mut history = HistoryTable::new();
        history.record(&board, empty, 4);
        assert_eq!(history.value(&board, empty), 0);
    }

    /// ⚠️ **Every (side, kind, destination) needs an entry nothing else
    /// shares**, and every other board here holds unpromoted black pieces in
    /// the middle of the table — a wrong factor in [`HistoryTable::index`]
    /// serves all of those and runs off the end the first time a promoted
    /// piece cuts a node off in a real game.
    ///
    /// The check is the reading *before* each record: a triple that landed on
    /// an entry another one already used would find it non-zero. Nothing here
    /// recomputes the index arithmetic, which would test only itself.
    ///
    /// Sabotage: add `index`'s two piece factors rather than scaling by
    /// `PieceKind::NUM`, or add the destination rather than scaling by
    /// `Square::NUM` — this goes red alone on either. ⚠️ Dropping the
    /// piece-kind factor from the length `new` allocates reddens forty-two
    /// tests in this crate instead, this one among them, because the index
    /// then runs off the end of the table rather than colliding inside it.
    #[test]
    fn every_side_kind_and_destination_has_an_entry_of_its_own() {
        let from = Square::new(5, 5).expect("5e");
        let mut history = HistoryTable::new();
        let mut recorded = 0;

        for color in Color::all() {
            for kind in PieceKind::all() {
                // Legality is not the question — `index` reads the piece off
                // the origin — so the board is one piece and nothing else,
                // which also keeps every destination empty and every move
                // quiet.
                let mut board = PartialPosition::empty();
                board.piece_set(from, Some(Piece::new(kind, color)));
                let board = Position::new(board);

                for to in Square::all().filter(|to| *to != from) {
                    let mv = Move::Normal {
                        from,
                        to,
                        promote: false,
                    };
                    assert_eq!(
                        history.value(&board, mv),
                        0,
                        "{color:?} {kind:?} to {to:?} shares an entry"
                    );
                    history.record(&board, mv, 4);
                    recorded += 1;
                }
            }
        }
        assert_eq!(
            recorded,
            Color::NUM * PieceKind::NUM * (Square::NUM - 1),
            "the walk did not cover the index"
        );
    }
}
