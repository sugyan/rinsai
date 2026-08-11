//! A game in progress: the board, its history, and the USI text that describes
//! it.
//!
//! shunsai holds no game history by design, and it can neither read nor write
//! SFEN. Both gaps land here, and both land at *game* level rather than inside
//! the search:
//!
//! * `shogi_core` has no unmake, so keeping its board in lockstep *inside* a
//!   search would mean saving and restoring ~368 bytes per node. At game level
//!   it is one `make_move` per move actually played — a few hundred per game,
//!   against a search doing millions of nodes.
//! * 千日手 is a rule about a *game*, not a position, so the type that owns the
//!   history is the one named for it.
//!
//! The record half is **`shogi_core::Position`**, imported here as [`Record`].
//! It is exactly `{ initial, inner, moves }` with a `make_move` that advances
//! the board and pushes the move, which is precisely the bookkeeping a game
//! needs — so it is delegated to rather than reimplemented. What is *not* used
//! is its `from_usi`: see [`Game::from_usi_position`].

use core::fmt;

use shogi_core::{Color, Hand, Move, PartialPosition, Piece, PieceKind, Square, ToUsi};
use shogi_usi_parser::FromUsi;
use shunsai::Position;

use crate::moves::is_legal;

/// `shogi_core::Position` — the *record* of a game (root position, current
/// position, moves played), as distinct from [`Position`], which throughout
/// rinsai means `shunsai::Position`, the board a search actually walks.
///
/// Aliased rather than imported under its own name, because the alias says
/// which of the two jobs this one does.
type Record = shogi_core::Position;

/// What repetition detection needs to know about a position that has occurred.
///
/// CLAUDE.md names this exact triple: `key` filters, hand equality confirms,
/// and the `in_check` run decides the perpetual-check case, where the checking
/// side loses. The crate's `repetition` module is what reads it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HistoryEntry {
    /// shunsai's incremental Zobrist key: board, hands and side to move, but
    /// *not* ply — which is exactly what makes it usable as a repetition filter.
    pub key: u64,
    /// Confirms a `key` match. Confirmation, not proof: only a full board
    /// compare would be proof, and CLAUDE.md prescribes this pair.
    pub hands: [Hand; 2],
    /// Whether the side to move was in check.
    pub in_check: bool,
}

/// Everything that can go wrong building a position from USI text.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PositionError {
    /// `position` with no argument at all.
    MissingRoot,
    /// The `startpos` / `sfen …` part did not parse.
    BadRoot { detail: String },
    /// Something other than `moves` followed the root.
    UnexpectedToken { index: usize, token: String },
    /// A token in the `moves` list is not USI move notation.
    MoveSyntax { index: usize, token: String },
    /// A move parsed but is not legal in the position it was reached in.
    IllegalMove { index: usize, token: String },
}

impl fmt::Display for PositionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRoot => f.write_str("expected `startpos` or `sfen …`"),
            Self::BadRoot { detail } => write!(f, "bad root position: {detail}"),
            Self::UnexpectedToken { index, token } => {
                write!(f, "expected `moves`, found `{token}` at token {index}")
            }
            Self::MoveSyntax { index, token } => {
                write!(f, "move {index} `{token}` is not USI move notation")
            }
            Self::IllegalMove { index, token } => {
                write!(f, "move {index} `{token}` is not legal in this position")
            }
        }
    }
}

impl std::error::Error for PositionError {}

/// `mv` is not legal in the position it was played in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IllegalMove(pub Move);

impl fmt::Display for IllegalMove {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not legal in this position",
            self.0.to_usi_owned()
        )
    }
}

impl std::error::Error for IllegalMove {}

/// A shogi game: a root position, the moves played from it, and the board they
/// reach.
pub struct Game {
    /// The search-facing board, and the source of truth for anything the search
    /// asks: shunsai is the only one of the two with unmake and an incremental
    /// Zobrist key.
    board: Position,
    /// The record, advanced in lockstep with `board`. It lets the game emit
    /// SFEN — shunsai deliberately cannot — and it *is* the `position`
    /// command's own semantics: the root plus the moves played. ⚠️ The plies
    /// before a mid-game `sfen` root are unknowable, and `initial_position()`
    /// is what says so.
    record: Record,
    /// One entry per position *reached*, including the root, so
    /// `history.len() == moves().len() + 1`.
    history: Vec<HistoryEntry>,
}

impl Game {
    /// The initial position, no moves played.
    #[must_use]
    pub fn from_startpos() -> Self {
        Self::from_partial(PartialPosition::startpos())
            .expect("the initial position is a valid shogi position")
    }

    /// A game rooted at `partial`, no moves played.
    ///
    /// ⚠️ **Fallible on purpose, and this is a crash the wire can reach.**
    /// `PartialPosition` is only a container: `shogi_usi_parser` builds one from
    /// `sfen … b 19P 1` without complaint, because its hand grammar accepts a
    /// two-digit count and `Hand::added` has no cap. Handing that to shunsai is
    /// fatal — its Zobrist table is `[u64; 19]` per piece kind, indexed by held
    /// count, so the nineteenth piece is an out-of-bounds panic. A GUI or a
    /// server could end the process with one line.
    pub fn from_partial(partial: PartialPosition) -> Result<Self, PositionError> {
        check_piece_counts(&partial)?;
        let board = Position::new(partial.clone());
        let root = HistoryEntry::of(&board);
        Ok(Self {
            board,
            record: Record::arbitrary_position(partial),
            history: vec![root],
        })
    }

    /// Parses a USI `position` argument — everything after the command word:
    /// `startpos [moves …]` or `sfen <board> <stm> <hands> [<ply>] [moves …]`.
    ///
    /// Every move is parsed *and checked for legality* before it is played.
    ///
    /// ⚠️ **`shogi_core::Position::from_usi` does this in one call and is not
    /// used**, for two reasons narrower than "it does not report errors" — a
    /// *malformed* token does produce one. A move that is well-formed but
    /// cannot be made (nothing on the from square, wrong side to move) is
    /// **silently dropped** and success reported; a move that is structurally
    /// fine but illegal (二歩, moving into check) is **applied**, because
    /// `make_move` documents that it never checks legality. Either way the
    /// search runs on a position nobody described.
    /// `tests/shogi_core_from_usi.rs` pins both down, so the day `shogi_core`
    /// changes, that test fails and this decision is revisited.
    ///
    /// Its SFEN half *is* used: the prefix goes to `PartialPosition::from_usi`.
    pub fn from_usi_position(args: &str) -> Result<Self, PositionError> {
        let tokens: Vec<&str> = args.split_whitespace().collect();
        let (root_src, rest) = split_root(&tokens)?;

        let partial = PartialPosition::from_usi(&root_src).map_err(|e| PositionError::BadRoot {
            detail: format!("{e:?}"),
        })?;
        let mut game = Self::from_partial(partial)?;

        let move_tokens = match rest.split_first() {
            None => &[][..],
            Some((&"moves", tail)) => tail,
            Some((&token, _)) => {
                return Err(PositionError::UnexpectedToken {
                    index: tokens.len() - rest.len(),
                    token: token.to_owned(),
                });
            }
        };
        for (index, token) in move_tokens.iter().enumerate() {
            game.push_usi_move(token)
                .map_err(|e| e.with_index(index + 1))?;
        }
        Ok(game)
    }

    /// Parses one USI move token in the current position and plays it.
    ///
    /// The index in any error is `0`; [`Self::from_usi_position`] rewrites it.
    pub fn push_usi_move(&mut self, token: &str) -> Result<Move, PositionError> {
        let parsed = Move::from_usi(token).map_err(|_| PositionError::MoveSyntax {
            index: 0,
            token: token.to_owned(),
        })?;
        // USI drop notation carries no colour — `P*7f` is the same text for
        // either side — so `shogi_usi_parser` hard-codes Black and documents it
        // (`src/mv.rs:5-7`). The colour has to come from the side to move.
        let mv = match parsed {
            Move::Drop { piece, to } => Move::Drop {
                piece: Piece::new(piece.piece_kind(), self.board.side_to_move()),
                to,
            },
            normal => normal,
        };
        self.push_move(mv).map_err(|_| PositionError::IllegalMove {
            index: 0,
            token: token.to_owned(),
        })?;
        Ok(mv)
    }

    /// Plays `mv`, or reports that it is not legal here.
    ///
    /// This check is what keeps [`Position::do_move`]'s documented `expect`s
    /// unreachable from anything a GUI or a server can send.
    pub fn push_move(&mut self, mv: Move) -> Result<(), IllegalMove> {
        if !is_legal(&self.board, mv) {
            return Err(IllegalMove(mv));
        }
        self.board.do_move(mv);
        // A move shunsai has just called legal cannot fail `make_move`'s
        // structural checks, so a `None` here means the two have drifted apart.
        let applied = self.record.make_move(mv);
        debug_assert!(
            applied.is_some(),
            "the lockstep record rejected a legal move: {mv:?}"
        );
        self.history.push(HistoryEntry::of(&self.board));
        Ok(())
    }

    /// The board. Deliberately no `&mut` counterpart: a search that needs to
    /// move pieces takes [`Self::search_board`], so "the search must leave the
    /// position balanced" is not an invariant anyone here can violate.
    #[must_use]
    pub fn position(&self) -> &Position {
        &self.board
    }

    /// A board of the search's own, **rebuilt from the record** rather than
    /// copied — the board a searcher does its do/undo on.
    ///
    /// ⚠️ [`Position::clone`] would deep-copy shunsai's undo stack. Rebuilding
    /// hands the search an *empty* one, so a search that unwinds past its own
    /// root trips `undo_move`'s `expect` loudly instead of quietly walking into
    /// a position nobody described.
    ///
    /// A method rather than a `position().clone()` at the call site, because
    /// today's caller only *happens* to hold an already-cloned game with an
    /// empty stack. `SearchJob` is public and E3's self-play driver is its
    /// second caller; a job built straight from a game fifty moves in would
    /// both copy fifty undo entries and lose the unwind guarantee.
    #[must_use]
    pub fn search_board(&self) -> Position {
        let board = Position::new(self.record.inner().clone());
        debug_assert_eq!(
            board.key(),
            self.board.key(),
            "the lockstep record and the incremental Zobrist key disagree"
        );
        debug_assert_eq!(board.ply(), self.board.ply());
        board
    }

    /// The current position in SFEN, without the `sfen` keyword.
    ///
    /// `O(1)` — this is what keeping the record in lockstep buys.
    #[must_use]
    pub fn sfen(&self) -> String {
        self.record.to_sfen_owned()
    }

    /// The root position in SFEN, without the `sfen` keyword.
    #[must_use]
    pub fn initial_sfen(&self) -> String {
        self.record.initial_position().to_sfen_owned()
    }

    /// Every move played from the root, in order.
    #[must_use]
    pub fn moves(&self) -> &[Move] {
        self.record.moves()
    }

    /// One entry per position reached, root first; `history().len()` is always
    /// `moves().len() + 1`.
    #[must_use]
    pub fn history(&self) -> &[HistoryEntry] {
        &self.history
    }

    #[must_use]
    pub fn side_to_move(&self) -> Color {
        self.board.side_to_move()
    }

    /// Whether the side to move is in check.
    #[must_use]
    pub fn in_check(&self) -> bool {
        self.board.in_check()
    }
}

impl HistoryEntry {
    /// The entry for the position `board` is in.
    ///
    /// ⚠️ **The search builds its own path with this too**, rather than with a
    /// second constructor of its own: the game's history and the search's
    /// continuation of it are compared against each other entry for entry, so
    /// two ways of building one would be two ways for them to disagree.
    pub(crate) fn of(board: &Position) -> Self {
        Self {
            key: board.key(),
            hands: [board.hand(Color::Black), board.hand(Color::White)],
            in_check: board.in_check(),
        }
    }
}

/// Rebuilds the board from the record rather than copying it — see
/// [`Game::search_board`], which this delegates to and which carries the
/// argument.
///
/// The rebuild also cross-checks the from-scratch key against the
/// incrementally maintained one, which makes the lockstep record a checked
/// property rather than a hope.
///
/// ⚠️ **It is a `debug_assert`, compiled out of the release binary that
/// actually plays**, so the guarantee is "the test suite would have caught a
/// drift", not "a live game will". Kept that way deliberately: promoting it
/// would put a panic on the protocol thread once per `go`. DECISIONS.md
/// carries the trade and what would reopen it.
impl Clone for Game {
    fn clone(&self) -> Self {
        Self {
            board: self.search_board(),
            record: self.record.clone(),
            history: self.history.clone(),
        }
    }
}

impl fmt::Debug for Game {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Game {{ sfen {}", self.initial_sfen())?;
        if !self.moves().is_empty() {
            f.write_str(" moves")?;
            for mv in self.moves() {
                write!(f, " {}", mv.to_usi_owned())?;
            }
        }
        f.write_str(" }")
    }
}

/// How many of each kind a shogi set contains, promoted pieces counting as the
/// kind they were promoted from.
///
/// ⚠️ The king is **not** here: its bound is per colour rather than per set —
/// a total of two admits two black kings and no white one — so
/// [`check_piece_counts`] handles it separately.
const PIECE_TOTALS: [(PieceKind, u8); 7] = [
    (PieceKind::Pawn, 18),
    (PieceKind::Lance, 4),
    (PieceKind::Knight, 4),
    (PieceKind::Silver, 4),
    (PieceKind::Gold, 4),
    (PieceKind::Bishop, 2),
    (PieceKind::Rook, 2),
];

/// Rejects a root that could not come from a real shogi set.
///
/// One check covers two routes to the same crash: a hand count the parser
/// accepted but no set contains (`19P`), and — with no malformed field at all —
/// 18 pawns in hand plus one on the board, where capturing that pawn tips
/// shunsai's hand counter over. Bounding the *total* per kind at the root
/// closes both, because no legal move can create a piece.
///
/// ⚠️ The king is bounded **per colour** — it never changes sides, so a per-set
/// total of two would admit two black kings and no white one — and at *most*
/// one rather than exactly one, because a 詰将棋 diagram routinely omits the
/// attacking king. shunsai's `king_square` returns `Option` and documents
/// `None` as legal.
fn check_piece_counts(partial: &PartialPosition) -> Result<(), PositionError> {
    let mut seen = [0u16; PieceKind::NUM];
    let mut kings = [0u16; 2];
    for square in Square::all() {
        if let Some(piece) = partial.piece_at(square) {
            let kind = piece.piece_kind();
            let base = kind.unpromote().unwrap_or(kind);
            seen[base.array_index()] += 1;
            if base == PieceKind::King {
                kings[piece.color().array_index()] += 1;
            }
        }
    }
    for color in Color::all() {
        let hand = partial.hand_of_a_player(color);
        for kind in Hand::all_hand_pieces() {
            seen[kind.array_index()] += u16::from(hand.count(kind).unwrap_or(0));
        }
    }
    for (kind, total) in PIECE_TOTALS {
        let count = seen[kind.array_index()];
        if count > u16::from(total) {
            return Err(PositionError::BadRoot {
                detail: format!("{count} {kind:?} on the board and in hand, but a set has {total}"),
            });
        }
    }
    for color in Color::all() {
        let count = kings[color.array_index()];
        if count > 1 {
            return Err(PositionError::BadRoot {
                detail: format!("{count} {color:?} kings, but a set has one each"),
            });
        }
    }
    Ok(())
}

/// Splits `startpos` / `sfen …` off the front, returning it as a string
/// `PartialPosition::from_usi` accepts plus whatever follows.
fn split_root<'a>(tokens: &'a [&'a str]) -> Result<(String, &'a [&'a str]), PositionError> {
    match tokens.split_first() {
        None => Err(PositionError::MissingRoot),
        Some((&"startpos", rest)) => Ok(("startpos".to_owned(), rest)),
        Some((&"sfen", rest)) => {
            // The board, side to move, hands and an optional move number —
            // then either the end or `moves`.
            let field_count = rest.iter().take_while(|t| **t != "moves").count().min(4);
            if field_count < 3 {
                return Err(PositionError::BadRoot {
                    detail: format!("expected 3 or 4 SFEN fields, found {field_count}"),
                });
            }
            let (fields, rest) = rest.split_at(field_count);
            // ⚠️ Checked here rather than left to
            // `PartialPosition::from_usi`, which **saturates and discards**:
            // `0` becomes 1 and 65536+ becomes 65535, reporting success either
            // way, while a *non-numeric* field is rejected.
            if let Some(ply) = fields.get(3) {
                match ply.parse::<u16>() {
                    Ok(n) if n >= 1 => {}
                    _ => {
                        return Err(PositionError::BadRoot {
                            detail: format!("move number `{ply}` is not in 1..=65535"),
                        });
                    }
                }
            }
            Ok((format!("sfen {}", fields.join(" ")), rest))
        }
        Some((&token, _)) => Err(PositionError::UnexpectedToken {
            index: 0,
            token: token.to_owned(),
        }),
    }
}

impl PositionError {
    fn with_index(self, index: usize) -> Self {
        match self {
            Self::MoveSyntax { token, .. } => Self::MoveSyntax { index, token },
            Self::IllegalMove { token, .. } => Self::IllegalMove { index, token },
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use shogi_core::{PieceKind, Square};

    use super::*;

    const STARTPOS_SFEN: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

    #[test]
    fn startpos_and_its_sfen_agree() {
        let game = Game::from_usi_position("startpos").expect("startpos parses");
        assert_eq!(game.sfen(), STARTPOS_SFEN);
        assert_eq!(game.side_to_move(), Color::Black);
        assert_eq!(game.moves().len(), 0);
        assert_eq!(game.history().len(), 1);
    }

    #[test]
    fn an_sfen_root_round_trips() {
        let sfen = "sfen l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1";
        let game = Game::from_usi_position(sfen).expect("SFEN parses");
        assert_eq!(format!("sfen {}", game.sfen()), sfen);
        assert_eq!(game.side_to_move(), Color::White);
    }

    /// The move number is optional in SFEN, and `moves` may follow either form.
    #[test]
    fn the_sfen_move_number_is_optional() {
        let with = Game::from_usi_position(&format!("sfen {STARTPOS_SFEN} moves 7g7f"))
            .expect("with a ply");
        let without = Game::from_usi_position(
            "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - moves 7g7f",
        )
        .expect("without a ply");
        assert_eq!(with.sfen(), without.sfen());
    }

    #[test]
    fn moves_are_applied_in_order() {
        let game = Game::from_usi_position("startpos moves 7g7f 3c3d 8h2b+ 3a2b")
            .expect("a real opening parses");
        assert_eq!(game.moves().len(), 4);
        assert_eq!(game.history().len(), 5);
        assert_eq!(game.side_to_move(), Color::Black);

        // 8h2b+ captured a bishop and promoted; 3a2b recaptured with the silver.
        let two_b = Square::new(2, 2).expect("2b");
        assert_eq!(
            game.position().piece_at(two_b),
            Some(Piece::new(PieceKind::Silver, Color::White))
        );
        assert_eq!(
            game.position().hand(Color::Black).count(PieceKind::Bishop),
            Some(1)
        );
        assert_eq!(
            game.position().hand(Color::White).count(PieceKind::Bishop),
            Some(1)
        );
    }

    /// USI drop notation carries no colour. Sabotage: remove the colour rewrite
    /// in `push_usi_move` and `P*9b` becomes a *black* pawn drop that Black has
    /// not got in hand — `is_legal` rejects it, so the whole command fails and
    /// this test goes red rather than the board quietly gaining an enemy pawn.
    #[test]
    fn drops_take_the_side_to_move_colour() {
        let game = Game::from_usi_position("sfen 9/9/9/9/9/9/9/9/K7k b Rp 1 moves R*9a P*9b")
            .expect("both drops are legal");
        assert_eq!(
            game.position().piece_at(Square::new(9, 1).expect("9a")),
            Some(Piece::new(PieceKind::Rook, Color::Black))
        );
        assert_eq!(
            game.position().piece_at(Square::new(9, 2).expect("9b")),
            Some(Piece::new(PieceKind::Pawn, Color::White))
        );
    }

    /// ⚠️ It asserts the *error*, not the surviving board, and cannot do
    /// otherwise: `from_usi_position` is a constructor, so no `Game` escapes on
    /// the error path. That a rejected command leaves the engine's own board
    /// untouched is `usi::set_position`'s property, covered by
    /// `usi_conformance::an_illegal_move_rejects_the_whole_position_command`.
    #[test]
    fn an_illegal_move_fails_the_whole_command() {
        // 5e is empty in every position reachable from startpos in two moves.
        let err = Game::from_usi_position("startpos moves 7g7f 3c3d 5e5d 8h2b+")
            .expect_err("5e5d is not legal");
        assert_eq!(
            err,
            PositionError::IllegalMove {
                index: 3,
                token: "5e5d".to_owned()
            }
        );
    }

    #[test]
    fn a_malformed_move_token_is_reported_with_its_index() {
        let err =
            Game::from_usi_position("startpos moves 7g7f zzzz").expect_err("`zzzz` is not a move");
        assert_eq!(
            err,
            PositionError::MoveSyntax {
                index: 2,
                token: "zzzz".to_owned()
            }
        );
    }

    /// ⚠️ A GUI or a server could end the process with one line before this
    /// existed — see [`Game::from_partial`].
    ///
    /// Sabotage: delete the `check_piece_counts` call in `from_partial`. ⚠️
    /// Only the three two-digit-hand cases panic (shunsai's `zobrist.rs`
    /// indexes `[u64; 19]` by held count); the other four are silently
    /// **accepted** and fail the assertion below instead. Those four are the
    /// indirect route — no malformed field anywhere — that this check exists
    /// for.
    #[test]
    fn a_position_no_shogi_set_could_reach_is_rejected() {
        for sfen in [
            // Straight out of the hand grammar.
            "sfen 4k4/9/9/9/9/9/9/9/4K4 b 19P 1",
            "sfen 4k4/9/9/9/9/9/9/9/4K4 b 99P 1",
            "sfen 4k4/9/9/9/9/9/9/9/4K4 w 19p 1",
            "sfen 4k4/9/9/9/9/9/9/9/4K4 b 5R 1",
            // The indirect route: 18 in hand plus one on the board is 19, and
            // capturing it is what used to tip shunsai's counter over.
            "sfen 4k4/9/9/9/4p4/9/9/9/4K4 b 18P 1",
            // Promoted pieces count as what they were: a +P on the board plus
            // 18 in hand is nineteen pawns. Sabotage: drop the `unpromote()`
            // in `check_piece_counts` and this line alone is *accepted* — so
            // the assertion below fires — because `ProPawn` is not a key in
            // `PIECE_TOTALS` and the pawn on the board stops being counted.
            "sfen 4k4/9/9/9/4+P4/9/9/9/4K4 b 18P 1",
            // Golds, to show the bound is per kind and not only about pawns.
            "sfen 3kg4/9/9/9/9/9/9/9/3KG4 b 4G 1",
        ] {
            assert!(
                matches!(
                    Game::from_usi_position(sfen),
                    Err(PositionError::BadRoot { .. })
                ),
                "accepted an impossible position: {sfen}"
            );
        }
    }

    /// The bound is on the total, so a set that is merely *unusual* still works.
    #[test]
    fn a_legal_but_lopsided_position_is_accepted() {
        for sfen in [
            "sfen 4k4/9/9/9/9/9/9/9/4K4 b 18P 1",
            "sfen 4k4/9/9/9/9/9/9/9/4K4 b 2R2B4G4S4N4L18P 1",
            "sfen l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1",
        ] {
            assert!(
                Game::from_usi_position(sfen).is_ok(),
                "rejected a reachable position: {sfen}"
            );
        }
    }

    /// The king is the one kind that cannot change sides, so its bound is per
    /// colour.
    ///
    /// ⚠️ **The first fixture is the only one that discriminates.** The other
    /// two hold *three* kings, which a per-set total of two rejects on the
    /// total alone; two black kings and no white one is the position only a
    /// per-colour bound catches.
    ///
    /// Sabotage: put `(PieceKind::King, 2)` back into `PIECE_TOTALS` and drop
    /// the per-colour loop, and the first case is accepted.
    #[test]
    fn two_kings_of_one_colour_are_rejected() {
        for sfen in [
            "sfen 9/9/9/9/9/9/9/9/3KK4 b - 1",
            "sfen 4k4/9/9/9/9/9/9/9/3KK4 b - 1",
            "sfen 3kk4/9/9/9/9/9/9/9/4K4 b - 1",
        ] {
            assert!(
                matches!(
                    Game::from_usi_position(sfen),
                    Err(PositionError::BadRoot { .. })
                ),
                "accepted two kings of one colour: {sfen}"
            );
        }
    }

    /// …but *at most* one, not exactly one — see [`check_piece_counts`].
    #[test]
    fn a_position_with_no_king_for_one_side_is_accepted() {
        for sfen in [
            // The shape a tsume diagram arrives in: no black king at all.
            "sfen 4k4/9/4P4/9/9/9/9/9/9 b G2r2b3g4s4n4l17p 1",
            // And the empty board, which is what `9/9/…` degenerates to.
            "sfen 9/9/9/9/9/9/9/9/9 b - 1",
        ] {
            assert!(
                Game::from_usi_position(sfen).is_ok(),
                "rejected a king-less position: {sfen}"
            );
        }
    }

    /// See `split_root`: `PartialPosition::from_usi` saturates and discards
    /// this field rather than rejecting it.
    #[test]
    fn an_out_of_range_move_number_is_rejected_rather_than_clamped() {
        let board = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -";
        for bad in ["0", "65536", "99999999999999999999", "-1", "1.5"] {
            assert!(
                matches!(
                    Game::from_usi_position(&format!("sfen {board} {bad}")),
                    Err(PositionError::BadRoot { .. })
                ),
                "accepted move number `{bad}`"
            );
        }
        for good in ["1", "2", "65535"] {
            assert!(
                Game::from_usi_position(&format!("sfen {board} {good}")).is_ok(),
                "rejected move number `{good}`"
            );
        }
        // Still optional.
        assert!(Game::from_usi_position(&format!("sfen {board}")).is_ok());
    }

    #[test]
    fn a_bad_root_is_reported_rather_than_guessed_at() {
        assert_eq!(
            Game::from_usi_position("").unwrap_err(),
            PositionError::MissingRoot
        );
        assert!(matches!(
            Game::from_usi_position("sfen not/a/real/sfen b - 1"),
            Err(PositionError::BadRoot { .. })
        ));
        assert!(matches!(
            Game::from_usi_position("sfen 9/9/9"),
            Err(PositionError::BadRoot { .. })
        ));
        assert!(matches!(
            Game::from_usi_position("frobnicate"),
            Err(PositionError::UnexpectedToken { .. })
        ));
        assert!(matches!(
            Game::from_usi_position("startpos mvoes 7g7f"),
            Err(PositionError::UnexpectedToken { .. })
        ));
    }

    /// `position startpos moves` with an empty list is what a GUI sends on the
    /// first move of a game.
    #[test]
    fn an_empty_moves_list_is_accepted() {
        let game = Game::from_usi_position("startpos moves").expect("an empty list is fine");
        assert_eq!(game.sfen(), STARTPOS_SFEN);
    }

    /// The history is one entry per position reached, and it records the check
    /// flag 連続王手の千日手 is decided from.
    #[test]
    fn the_history_records_every_position_reached() {
        let game = Game::from_usi_position("startpos moves 7g7f 3c3d 8h3c+")
            .expect("8h3c+ is a legal bishop capture-promotion");
        assert_eq!(game.history().len(), game.moves().len() + 1);

        let entries = game.history();
        assert!(!entries[0].in_check);
        // The entry for the final position must agree with the live board.
        assert_eq!(entries[3].in_check, game.in_check());
        assert_eq!(entries[3].key, game.position().key());
        assert_eq!(
            entries[3].hands,
            [
                game.position().hand(Color::Black),
                game.position().hand(Color::White)
            ]
        );
    }

    /// The record's *root* must not move while its current position does. Step
    /// 4 needs it to know where the recorded history begins.
    #[test]
    fn the_record_keeps_its_root_while_the_board_advances() {
        let mut game = Game::from_usi_position("startpos").expect("startpos parses");
        let root = game.sfen();
        assert_eq!(game.initial_sfen(), root);

        for token in ["7g7f", "3c3d", "8h2b+", "3a2b", "B*5e"] {
            game.push_usi_move(token)
                .unwrap_or_else(|e| panic!("{token}: {e}"));
        }
        assert_eq!(game.initial_sfen(), root, "the root moved");
        assert_ne!(game.sfen(), root);
        assert_eq!(game.moves().len(), 5);
        assert_eq!(game.history().len(), 6);
    }

    /// Sabotage: delete the `record.make_move` call in `push_move` and this
    /// fires — the clone rebuilds the board from the record and compares its
    /// from-scratch key against the incrementally maintained one.
    #[test]
    fn cloning_cross_checks_the_lockstep_board() {
        // The bishop trade leaves one in each hand, so the drop exercises the
        // hand half of the Zobrist key as well as the board half.
        let game = Game::from_usi_position("startpos moves 7g7f 3c3d 8h2b+ 3a2b B*5e")
            .expect("a legal sequence");
        let copy = game.clone();
        assert_eq!(copy.sfen(), game.sfen());
        assert_eq!(copy.position().key(), game.position().key());
        assert_eq!(copy.position().ply(), game.position().ply());
        assert_eq!(copy.moves(), game.moves());
        assert_eq!(copy.history(), game.history());
    }

    #[test]
    fn debug_reads_back_as_the_position_command_that_built_it() {
        let game = Game::from_usi_position("startpos moves 7g7f 3c3d").expect("parses");
        assert_eq!(
            format!("{game:?}"),
            format!("Game {{ sfen {STARTPOS_SFEN} moves 7g7f 3c3d }}")
        );
    }
}
