//! Why `Game` replays `position … moves` itself instead of calling
//! `shogi_core::Position::from_usi`.
//!
//! That function parses the whole USI `position` argument in one call, and it
//! is tempting. These tests pin down what it actually does with input an engine
//! will really receive, next to what [`Game::from_usi_position`] does with the
//! same input, so the decision rests on measured behaviour rather than on a
//! reading of the source — and so that **if `shogi_core` ever changes, this
//! fails and the decision gets revisited** rather than being inherited forever.
//!
//! The summary, which is narrower than "it never reports errors":
//!
//! | input | `Position::from_usi` | `Game::from_usi_position` |
//! |---|---|---|
//! | malformed move token | `Err` | `Err` |
//! | valid syntax, impossible on the board | **`Ok`, move silently dropped** | `Err` |
//! | structurally fine but illegal (nifu, self-check) | **`Ok`, move applied** | `Err` |
//!
//! It is the second and third rows that disqualify it. The second hands the
//! engine a position the GUI never described; the third hands it one that is
//! not a shogi position at all. `shogi_core` is not at fault — `make_move`
//! documents "this function will never check legality", and legality checking
//! is explicitly out of that crate's scope. It is simply not the tool for the
//! command an engine has to trust.

use rinsai_search::shogi_core::{Color, Piece, PieceKind, Position, Square, ToUsi};
use rinsai_search::{Game, PositionError};
use shogi_usi_parser::FromUsi;

fn square(file: u8, rank: u8) -> Square {
    Square::new(file, rank).expect("a square on the board")
}

fn played(position: &Position) -> Vec<String> {
    position.moves().iter().map(|m| m.to_usi_owned()).collect()
}

/// The one case it *does* reject. Recorded because the opposite is easy to
/// assume from the loop's `return Ok(...)` arms: an unparseable token leaves
/// input unconsumed, and `from_usi` rejects trailing input.
#[test]
fn a_malformed_move_token_is_an_error_for_both() {
    assert!(Position::from_usi("startpos moves 7g7f zzzz").is_err());
    assert!(matches!(
        Game::from_usi_position("startpos moves 7g7f zzzz"),
        Err(PositionError::MoveSyntax { index: 2, .. })
    ));
}

/// `5e5d` is well-formed but there is nothing on 5e. `make_move` returns
/// `None`, the loop carries on, and the caller is told everything went fine.
#[test]
fn a_move_that_cannot_be_made_is_silently_dropped() {
    let position = Position::from_usi("startpos moves 7g7f 5e5d 3c3d")
        .expect("it reports success — that is the problem");
    assert_eq!(played(&position), ["7g7f", "3c3d"]);

    assert!(matches!(
        Game::from_usi_position("startpos moves 7g7f 5e5d 3c3d"),
        Err(PositionError::IllegalMove { index: 2, .. })
    ));
}

/// Same shape, and the one a real game produces: the GUI's move list and the
/// engine's board silently disagree from here on.
#[test]
fn a_move_by_the_wrong_side_is_silently_dropped() {
    let position = Position::from_usi("startpos moves 7g7f 7f7e").expect("it reports success");
    assert_eq!(played(&position), ["7g7f"]);

    assert!(matches!(
        Game::from_usi_position("startpos moves 7g7f 7f7e"),
        Err(PositionError::IllegalMove { index: 2, .. })
    ));
}

/// 二歩. Structurally a fine drop — a pawn leaves the hand and lands on an
/// empty square — so it is *applied*, leaving two black pawns on file 5.
#[test]
fn nifu_is_accepted_and_applied() {
    const SFEN: &str = "sfen 4k4/9/9/9/9/9/4P4/9/4K4 b P 1 moves P*5e";
    let position = Position::from_usi(SFEN).expect("it reports success");

    let black_pawn = Piece::new(PieceKind::Pawn, Color::Black);
    assert_eq!(position.piece_at(square(5, 5)), Some(black_pawn));
    assert_eq!(position.piece_at(square(5, 7)), Some(black_pawn));

    assert!(matches!(
        Game::from_usi_position(SFEN),
        Err(PositionError::IllegalMove { index: 1, .. })
    ));
}

/// Moving into check. Also applied: the black king ends on 5i, on the file the
/// white rook on 5e already sweeps.
#[test]
fn moving_into_check_is_accepted_and_applied() {
    const SFEN: &str = "sfen 4k4/9/9/9/4r4/9/9/9/3K5 b - 1 moves 6i5i";
    let position = Position::from_usi(SFEN).expect("it reports success");

    assert_eq!(
        position.piece_at(square(5, 9)),
        Some(Piece::new(PieceKind::King, Color::Black))
    );
    assert_eq!(
        position.piece_at(square(5, 5)),
        Some(Piece::new(PieceKind::Rook, Color::White))
    );

    assert!(matches!(
        Game::from_usi_position(SFEN),
        Err(PositionError::IllegalMove { index: 1, .. })
    ));
}

/// The part that *is* sound, and the part rinsai keeps using: the SFEN prefix
/// on its own. `Game::from_usi_position` hands that to `PartialPosition`, which
/// reports errors properly, and replays only the moves itself.
#[test]
fn the_sfen_prefix_parser_is_sound_and_is_the_part_we_keep() {
    use rinsai_search::shogi_core::PartialPosition;

    assert!(PartialPosition::from_usi("sfen not/a/real/sfen b - 1").is_err());
    assert!(PartialPosition::from_usi("startpos").is_ok());

    let good = "sfen l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1";
    let game = Game::from_usi_position(good).expect("a real middlegame parses");
    assert_eq!(format!("sfen {}", game.sfen()), good);
}
