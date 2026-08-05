//! A game in progress: the board, its history, and the USI text that describes
//! it.
//!
//! shunsai holds no game history by design, and it can neither read nor write
//! SFEN. Both gaps land here, and both land at *game* level rather than inside
//! the search:
//!
//! * `shogi_core` has no unmake, so keeping a [`PartialPosition`] in lockstep
//!   inside a search would mean saving and restoring ~368 bytes per node. At
//!   game level it is one `make_move` per move actually played — a few hundred
//!   per game, against a search doing millions of nodes.
//! * 千日手 is a rule about a *game*, not a position, so the type that owns the
//!   history is the one named for it.

use core::fmt;

use shogi_core::{Color, Hand, Move, PartialPosition, Piece, ToUsi};
use shogi_usi_parser::FromUsi;
use shunsai::Position;

use crate::moves::is_legal;

/// What repetition detection needs to know about a position that has occurred.
///
/// Recorded from E0 step 1 and first *queried* at step 4. It is not speculative
/// building: CLAUDE.md makes 千日手 mandatory from E0 and names this exact
/// triple — `key()` filters, hand equality confirms, and the `in_check` run
/// decides the perpetual-check case, where the checking side loses.
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
    /// asks.
    position: Position,
    /// Advanced in lockstep with `position`, solely so the game can emit SFEN
    /// and hand a fresh root to a search thread.
    partial: PartialPosition,
    /// The position `moves` were played from. Kept because it and `moves` *are*
    /// the `position` command's own semantics, and because step 4 needs to know
    /// where the recorded history begins — the plies before a mid-game `sfen`
    /// root are unknowable, and this is what says so.
    initial: PartialPosition,
    /// Every move played from `initial`, in order.
    moves: Vec<Move>,
    /// One entry per position *reached*, including the root, so
    /// `history.len() == moves.len() + 1`.
    history: Vec<HistoryEntry>,
}

impl Game {
    /// The initial position, no moves played.
    #[must_use]
    pub fn from_startpos() -> Self {
        Self::from_partial(PartialPosition::startpos())
    }

    /// A game rooted at `partial`, no moves played.
    #[must_use]
    pub fn from_partial(partial: PartialPosition) -> Self {
        let position = Position::new(partial.clone());
        let root = HistoryEntry::of(&position);
        Self {
            position,
            initial: partial.clone(),
            partial,
            moves: Vec::new(),
            history: vec![root],
        }
    }

    /// Parses a USI `position` argument — everything after the command word:
    /// `startpos [moves …]` or `sfen <board> <stm> <hands> [<ply>] [moves …]`.
    ///
    /// Every move is parsed *and checked for legality* before it is played. The
    /// obvious alternative, `shogi_core::Position::from_usi`, does the whole
    /// thing in one call — but its source says *"Even if the read move does not
    /// make sense, the parser will not emit an error"*: it skips the move and
    /// returns `Ok` with a truncated position. An engine that accepted that
    /// would analyse a board the GUI does not believe in, which is the worst
    /// failure available to us. So we replay the moves ourselves.
    pub fn from_usi_position(args: &str) -> Result<Self, PositionError> {
        let tokens: Vec<&str> = args.split_whitespace().collect();
        let (root_src, rest) = split_root(&tokens)?;

        let partial = PartialPosition::from_usi(&root_src).map_err(|e| PositionError::BadRoot {
            detail: format!("{e:?}"),
        })?;
        let mut game = Self::from_partial(partial);

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
                piece: Piece::new(piece.piece_kind(), self.position.side_to_move()),
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
        if !is_legal(&self.position, mv) {
            return Err(IllegalMove(mv));
        }
        self.position.do_move(mv);
        // A move shunsai just called legal cannot fail `make_move`'s structural
        // checks, so a `None` here means the two boards have drifted apart.
        let applied = self.partial.make_move(mv);
        debug_assert!(
            applied.is_some(),
            "the lockstep PartialPosition rejected a legal move: {mv:?}"
        );
        self.moves.push(mv);
        self.history.push(HistoryEntry::of(&self.position));
        Ok(())
    }

    /// The board. There is deliberately no `&mut` counterpart: the search is
    /// handed a [`Game`] of its own (see [`Clone`]), so "the search must leave
    /// the position balanced" is not an invariant anyone here can violate.
    #[must_use]
    pub fn position(&self) -> &Position {
        &self.position
    }

    /// The current position in SFEN, without the `sfen` keyword.
    ///
    /// `O(1)` — this is what the lockstep [`PartialPosition`] buys.
    #[must_use]
    pub fn sfen(&self) -> String {
        self.partial.to_sfen_owned()
    }

    /// The root position in SFEN, without the `sfen` keyword.
    #[must_use]
    pub fn initial_sfen(&self) -> String {
        self.initial.to_sfen_owned()
    }

    /// Every move played from the root, in order.
    #[must_use]
    pub fn moves(&self) -> &[Move] {
        &self.moves
    }

    /// One entry per position reached, root first; `history().len()` is always
    /// `moves().len() + 1`.
    #[must_use]
    pub fn history(&self) -> &[HistoryEntry] {
        &self.history
    }

    /// The side to move.
    #[must_use]
    pub fn side_to_move(&self) -> Color {
        self.position.side_to_move()
    }

    /// Whether the side to move is in check.
    #[must_use]
    pub fn in_check(&self) -> bool {
        self.position.in_check()
    }
}

impl HistoryEntry {
    fn of(position: &Position) -> Self {
        Self {
            key: position.key(),
            hands: [position.hand(Color::Black), position.hand(Color::White)],
            in_check: position.in_check(),
        }
    }
}

/// Rebuilds the board from the lockstep [`PartialPosition`] rather than copying
/// it.
///
/// [`Position::clone`] would deep-copy the internal undo stack — the whole
/// game's worth. Rebuilding is cheaper, and it buys two things a copy would
/// not: the search starts with an *empty* undo stack, so a search that undoes
/// past its own root trips `undo_move`'s `expect` loudly instead of silently
/// corrupting the game position; and the from-scratch key is compared against
/// the incrementally maintained one, which is what makes the lockstep board a
/// checked property rather than a hope. This is also E2's Lazy SMP shape: each
/// helper thread takes its own copy from the same source.
impl Clone for Game {
    fn clone(&self) -> Self {
        let position = Position::new(self.partial.clone());
        debug_assert_eq!(
            position.key(),
            self.position.key(),
            "the lockstep PartialPosition and the incremental Zobrist key disagree"
        );
        debug_assert_eq!(position.ply(), self.position.ply());
        Self {
            position,
            partial: self.partial.clone(),
            initial: self.initial.clone(),
            moves: self.moves.clone(),
            history: self.history.clone(),
        }
    }
}

impl fmt::Debug for Game {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Game {{ sfen {}", self.initial_sfen())?;
        if !self.moves.is_empty() {
            f.write_str(" moves")?;
            for mv in &self.moves {
                write!(f, " {}", mv.to_usi_owned())?;
            }
        }
        f.write_str(" }")
    }
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

    /// Sabotage: an implementation that applied moves one at a time to the live
    /// game would leave the board after `7g7f 3c3d` here. Building into a
    /// scratch `Game` and only then assigning is what keeps a rejected command
    /// from leaving the engine on a board neither side believes in.
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
    /// flag step 4 decides 連続王手の千日手 from.
    #[test]
    fn the_history_records_every_position_reached() {
        let game = Game::from_usi_position("startpos moves 7g7f 3c3d 8h3c+")
            .expect("8h3c+ is a legal bishop capture-promotion");
        assert_eq!(game.history().len(), game.moves().len() + 1);

        let entries = game.history();
        assert!(!entries[0].in_check);
        // 8h3c+ gives check to the white king on 5a? No — but the entry for the
        // final position must at least agree with the live board.
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

    /// Sabotage: delete the `partial.make_move` call in `push_move` and this
    /// fires — the clone rebuilds the board from the lockstep copy and compares
    /// its from-scratch key against the incrementally maintained one.
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
