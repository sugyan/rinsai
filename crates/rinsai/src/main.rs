use std::io;
use std::process::ExitCode;

use rinsai_search::NegamaxSearcher;

fn main() -> ExitCode {
    // Anything unrecognised is a warning, not a refusal: an engine that will
    // not start is useless to an operator halfway through a tournament.
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--version" | "-V" => {
                // ⚠️ The one legitimate direct write to stdout: no protocol is
                // running yet and the process exits immediately. Everything
                // else goes through `Output`, because stdout *is* the USI
                // channel and a stray `println!` corrupts a live game.
                #[allow(clippy::print_stdout)]
                {
                    println!("rinsai {}", env!("CARGO_PKG_VERSION"));
                }
                return ExitCode::SUCCESS;
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
