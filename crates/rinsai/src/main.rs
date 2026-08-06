use std::io;
use std::process::ExitCode;

use rinsai_search::PlaceholderSearcher;

fn main() -> ExitCode {
    // `--version` is what packagers and match harnesses ask for. Anything else
    // is a warning, not a refusal: an engine that will not start is useless to
    // an operator halfway through a tournament.
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--version" | "-V" => {
                // The one legitimate direct write to stdout in the program: no
                // protocol is running yet, and the process exits immediately.
                // Everything else goes through `Output`, which is what the
                // `print_stdout` lint is here to enforce — stdout *is* the USI
                // channel, and a stray `println!` corrupts a live game.
                #[allow(clippy::print_stdout)]
                {
                    println!("rinsai {}", env!("CARGO_PKG_VERSION"));
                }
                return ExitCode::SUCCESS;
            }
            other => eprintln!("rinsai: ignoring unrecognised argument `{other}`"),
        }
    }

    // E0 step 1 has no search. `PlaceholderSearcher` answers with a legal move
    // and is deleted at step 2. The layering test is that the only lines in this
    // crate that need to change then are the ones *naming* the searcher — this
    // one, and `dialogue` in tests/usi_conformance.rs. Anything else changing
    // means step 1 got the seam wrong, and that is worth recording.
    // `stdout()` rather than `stdout().lock()`: the lock guard is not `Send`,
    // and the handle has to reach the search thread. `Output` serialises writes
    // itself, so the extra internal lock costs nothing at USI's line volume.
    rinsai::usi::run(io::stdin().lock(), io::stdout(), PlaceholderSearcher);
    ExitCode::SUCCESS
}
