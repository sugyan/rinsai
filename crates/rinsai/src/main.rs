use std::io;
use std::process::ExitCode;

use rinsai_search::NegamaxSearcher;

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

    // Step 1 shipped a placeholder here and predicted that swapping in a real
    // search would *need* to change only the lines that name the searcher. It
    // held: six lines across the crate, every one of them naming it or pointing
    // at where it lives — the `use` and the call below, and in
    // tests/usi_conformance.rs the `use`, `dialogue`, its doc line and the
    // module doc's pointer. Nothing else was forced. (Step 2 also *added* tests
    // to that file; nothing made it, and PROGRESS.md keeps the two apart,
    // because "nothing else changed" and "nothing else had to" are different
    // claims and only the second one is the result.)
    // `stdout()` rather than `stdout().lock()`: the lock guard is not `Send`,
    // and the handle has to reach the search thread. `Output` serialises writes
    // itself, so the extra internal lock costs nothing at USI's line volume.
    rinsai::usi::run(io::stdin().lock(), io::stdout(), NegamaxSearcher::new());
    ExitCode::SUCCESS
}
