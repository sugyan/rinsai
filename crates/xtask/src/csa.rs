//! A reader for floodgate CSA game records — the facts the openings pipeline
//! filters on, and nothing more.
//!
//! This is not a general CSA parser: it recognises move lines, the floodgate
//! rating comments and the `%`-termination, and ignores everything else
//! (board setup, `T<n>` per-move times, engine `'**` chatter). The `csa`
//! crate is not used because it discards comment lines, and the ratings this
//! pipeline filters on live in comments. Modelled on shunsai's
//! `examples/gen_bench_positions.rs` (MIT, same author).

use shogi_core::PieceKind;

/// One move statement, as the record wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CsaMove {
    /// `+` (Black) or `-` (White).
    pub black: bool,
    /// `(file, rank)`, or `None` for a drop (CSA writes the square `00`).
    pub from: Option<(u8, u8)>,
    /// `(file, rank)`.
    pub to: (u8, u8),
    /// ⚠️ The piece kind *after* the move — a promotion writes the promoted
    /// kind here, and the mover's kind has to be read off the board.
    pub kind: PieceKind,
}

/// What one record said, before any filtering.
#[derive(Debug, Clone)]
pub struct CsaGame {
    /// The file's basename, as `parse` was given it.
    pub name: String,
    pub moves: Vec<CsaMove>,
    /// From the floodgate comment `'black_rate:<name>:<value>`; `None` when
    /// the line is absent (unrated player).
    pub black_rate: Option<f64>,
    pub white_rate: Option<f64>,
    /// The last `%`-statement, e.g. `%TORYO` — `None` for a record that just
    /// stops.
    pub termination: Option<String>,
}

/// Read one record. Never fails: an unrecognised line is ignored, and what a
/// game is missing, the pipeline's filters decide on.
#[must_use]
pub fn parse(name: &str, text: &str) -> CsaGame {
    let mut game = CsaGame {
        name: name.to_owned(),
        moves: Vec::new(),
        black_rate: None,
        white_rate: None,
        termination: None,
    };
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("'black_rate:") {
            game.black_rate = rest.rsplit(':').next().and_then(|v| v.parse().ok());
        } else if let Some(rest) = line.strip_prefix("'white_rate:") {
            game.white_rate = rest.rsplit(':').next().and_then(|v| v.parse().ok());
        } else if line.starts_with('%') {
            let statement = line.split(',').next().unwrap_or(line);
            game.termination = Some(statement.to_owned());
        } else if let Some(mv) = parse_move(line) {
            game.moves.push(mv);
        }
    }
    game
}

/// A `[+-]XXYYPP` statement, with an inline `,T<secs>` suffix stripped if one
/// is present (floodgate writes times as separate `T<n>` lines instead, which
/// fall through as not-a-move).
fn parse_move(line: &str) -> Option<CsaMove> {
    let statement = line.split(',').next().unwrap_or(line);
    let bytes = statement.as_bytes();
    if bytes.len() != 7 || !(bytes[0] == b'+' || bytes[0] == b'-') {
        return None;
    }
    if !bytes[1..5].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let digit = |i: usize| bytes[i] - b'0';
    let square = |f: u8, r: u8| ((1..=9).contains(&f) && (1..=9).contains(&r)).then_some((f, r));

    let from = match (digit(1), digit(2)) {
        (0, 0) => None,
        (f, r) => Some(square(f, r)?),
    };
    let to = square(digit(3), digit(4))?;
    let kind = piece_kind(&statement[5..7])?;
    Some(CsaMove {
        black: bytes[0] == b'+',
        from,
        to,
        kind,
    })
}

/// The fourteen CSA piece codes.
fn piece_kind(code: &str) -> Option<PieceKind> {
    Some(match code {
        "FU" => PieceKind::Pawn,
        "KY" => PieceKind::Lance,
        "KE" => PieceKind::Knight,
        "GI" => PieceKind::Silver,
        "KI" => PieceKind::Gold,
        "KA" => PieceKind::Bishop,
        "HI" => PieceKind::Rook,
        "OU" => PieceKind::King,
        "TO" => PieceKind::ProPawn,
        "NY" => PieceKind::ProLance,
        "NK" => PieceKind::ProKnight,
        "NG" => PieceKind::ProSilver,
        "UM" => PieceKind::ProBishop,
        "RY" => PieceKind::ProRook,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_yields_its_moves_rates_and_termination() {
        let text = "V2\nN+alpha\nN-beta\n'Max_Moves:512\nP1-KY-KE-GI-KI-OU-KI-GI-KE-KY\n+\n\
                    'rating:alpha+0123:beta+4567\n\
                    'black_rate:alpha+0123:3789\n\
                    'white_rate:beta+4567:4168.5\n\
                    +7776FU\nT0\n'** 620 -3334FU\n-3334FU\nT12\n%TORYO\n\
                    'summary:toryo:alpha lose:beta win\n";
        let game = parse("a-record", text);
        assert_eq!(game.name, "a-record");
        assert_eq!(game.black_rate, Some(3789.0));
        assert_eq!(game.white_rate, Some(4168.5));
        assert_eq!(game.termination.as_deref(), Some("%TORYO"));
        assert_eq!(
            game.moves,
            vec![
                CsaMove {
                    black: true,
                    from: Some((7, 7)),
                    to: (7, 6),
                    kind: PieceKind::Pawn,
                },
                CsaMove {
                    black: false,
                    from: Some((3, 3)),
                    to: (3, 4),
                    kind: PieceKind::Pawn,
                },
            ]
        );
    }

    #[test]
    fn a_drop_has_no_from_square_and_an_inline_time_suffix_is_stripped() {
        let game = parse("g", "-0055KA,T3\n");
        assert_eq!(
            game.moves,
            vec![CsaMove {
                black: false,
                from: None,
                to: (5, 5),
                kind: PieceKind::Bishop,
            }]
        );
    }

    /// Every line here resembles a move enough to fool a looser matcher.
    #[test]
    fn what_is_not_a_move_is_not_taken_for_one() {
        for line in [
            "+",         // side-to-move marker
            "-",         // likewise
            "T0",        // per-move time
            "+7776XX",   // no such piece
            "+7770FU",   // rank 0
            "+0770FU",   // half a drop square
            "+77 6FU",   // digit missing
            "+7776FUFU", // too long
            "'+7776FU",  // commented out
        ] {
            let game = parse("g", line);
            assert!(game.moves.is_empty(), "{line:?} parsed as a move");
        }
    }

    /// The floodgate rate comment carries the player name, and player names
    /// may carry colons; the value is the *last* field.
    #[test]
    fn rates_survive_colons_in_player_names() {
        let game = parse(
            "g",
            "'black_rate:we:like:colons+9abc:2500\n'white_rate:beta+0:not-a-number\n",
        );
        assert_eq!(game.black_rate, Some(2500.0));
        assert_eq!(game.white_rate, None, "an unparseable value stays absent");
    }

    #[test]
    fn missing_rate_lines_stay_none() {
        let game = parse("g", "+7776FU\n%TORYO\n");
        assert_eq!(game.black_rate, None);
        assert_eq!(game.white_rate, None);
    }

    #[test]
    fn the_last_termination_statement_wins() {
        let game = parse("g", "%CHUDAN\n%SENNICHITE\n");
        assert_eq!(game.termination.as_deref(), Some("%SENNICHITE"));
    }

    #[test]
    fn crlf_records_read_the_same_as_lf_records() {
        let game = parse("g", "+7776FU\r\n'black_rate:a+0:3000\r\n%TORYO\r\n");
        assert_eq!(game.moves.len(), 1);
        assert_eq!(game.black_rate, Some(3000.0));
        assert_eq!(game.termination.as_deref(), Some("%TORYO"));
    }

    /// The committed fixture set, read through the parser, matches the
    /// catalogue below — one row per file, listing why each is in the set.
    /// The seven qualifying games feed the extractor's golden test; the four
    /// rejects cover three filter arms (rate twice — once too low, once
    /// absent — then termination and length); the second day exercises the
    /// day-list plumbing.
    #[test]
    fn the_committed_fixtures_read_as_catalogued() {
        // (day, basename fragment, black_rate, white_rate, termination, moves)
        type Row = (
            &'static str,
            &'static str,
            Option<f64>,
            Option<f64>,
            &'static str,
            usize,
        );
        #[rustfmt::skip]
        let catalogue: &[Row] = &[
            ("2026-06-01", "62KIN_Suisho11_4c+Allihn_Condenser+20260601130001", Some(3789.0), Some(4168.0), "%TORYO", 100),
            ("2026-06-01", "62KIN_Suisho11_4c+Pyfamate_v2_peta1115+20260601133002", Some(3789.0), Some(3502.0), "%TORYO", 105),
            ("2026-06-01", "62KIN_Suisho11_4c+clomeprop+20260601143005", Some(3792.0), Some(3788.0), "%TORYO", 114),
            ("2026-06-01", "62KIN_Suisho11_4c+jhbr2+20260601113000", Some(3861.0), Some(3267.0), "%TORYO", 71),
            ("2026-06-01", "62KIN_Suisho11_4c+kahm768+20260601080001", Some(3826.0), Some(4201.0), "%TORYO", 169),
            ("2026-06-01", "910+Gokaku-v2.3-b12c512-v500+20260601070006", Some(2045.0), Some(3155.0), "%TORYO", 86),
            ("2026-06-01", "C2AC562E-YO-NNUE-NOBOOK-20260510+jhbr2+20260601023003", Some(3108.0), Some(3270.0), "%TORYO", 29),
            ("2026-06-01", "Allihn_Condenser+62KIN_Suisho11_4c+20260601050005", Some(4164.0), Some(3791.0), "%SENNICHITE", 106),
            ("2026-06-01", "62KIN_Suisho11_4c+YukiSHogiEngineV7.7+20260601090001", Some(3823.0), None, "%TORYO", 102),
            ("2026-06-02", "62KIN_Suisho11_4c+Allihn_Condenser+20260602050002", Some(3847.0), Some(4168.0), "%TORYO", 82),
            ("2026-06-02", "62KIN_Suisho11_4c+ZARD+20260602160006", Some(3909.0), Some(3498.0), "%TORYO", 81),
        ];
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/floodgate");
        let mut seen = 0;
        for (day, fragment, black, white, termination, moves) in catalogue {
            let path = format!("{root}/{day}/wdoor+floodgate-300-10F+{fragment}.csa");
            let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
            let game = parse(fragment, &String::from_utf8_lossy(&bytes));
            assert_eq!(game.black_rate, *black, "{fragment}");
            assert_eq!(game.white_rate, *white, "{fragment}");
            assert_eq!(
                game.termination.as_deref(),
                Some(*termination),
                "{fragment}"
            );
            assert_eq!(game.moves.len(), *moves, "{fragment}");
            seen += 1;
        }
        // And nothing sits in the fixture tree uncatalogued.
        let mut on_disk = 0;
        for day in std::fs::read_dir(root).expect("fixture root exists") {
            for file in std::fs::read_dir(day.expect("readable").path()).expect("readable") {
                let file = file.expect("readable");
                if file.path().extension().is_some_and(|e| e == "csa") {
                    on_disk += 1;
                }
            }
        }
        assert_eq!(seen, on_disk, "every fixture file has a catalogue row");
    }

    #[test]
    fn all_fourteen_piece_codes_parse_and_a_fifteenth_does_not() {
        let codes = [
            "FU", "KY", "KE", "GI", "KI", "KA", "HI", "OU", "TO", "NY", "NK", "NG", "UM", "RY",
        ];
        for code in codes {
            assert!(piece_kind(code).is_some(), "{code} should map");
        }
        assert!(piece_kind("XX").is_none());
        assert!(piece_kind("fu").is_none(), "codes are upper-case");
    }
}
