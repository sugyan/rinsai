use std::io;
use std::process::ExitCode;

use rinsai_search::NegamaxSearcher;

fn main() -> ExitCode {
    // Anything unrecognised is a warning, not a refusal: an engine that will
    // not start is useless to an operator halfway through a tournament.
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" | "-V" => {
                // A direct write to stdout, legitimate for the same reason
                // `bench` is: no protocol is running and the process exits when
                // this returns. Everything a *session* writes goes through
                // `Output`, because stdout is the USI channel and a stray
                // `println!` corrupts a live game.
                #[allow(clippy::print_stdout)]
                {
                    println!("rinsai {}", env!("CARGO_PKG_VERSION"));
                }
                return ExitCode::SUCCESS;
            }
            // `bench [depth]`, the search analogue of perft. It ends the
            // process, so it can never overlap a session.
            "bench" => {
                let depth = match args.next() {
                    None => rinsai::bench::BENCH_DEPTH,
                    Some(value) => match value.parse() {
                        Ok(depth) => depth,
                        Err(_) => {
                            eprintln!("rinsai: bench: `{value}` is not a depth");
                            return ExitCode::FAILURE;
                        }
                    },
                };
                return if rinsai::bench::run(depth) {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                };
            }
            other => eprintln!("rinsai: ignoring unrecognised argument `{other}`"),
        }
    }

    // `stdout()` rather than `stdout().lock()`: the lock guard is not `Send`,
    // and the handle has to reach the search thread. `Output` serialises writes
    // itself, so the extra internal lock costs nothing at USI's line volume.
    rinsai::usi::run(io::stdin().lock(), io::stdout(), NegamaxSearcher::new());
    ExitCode::SUCCESS
}
