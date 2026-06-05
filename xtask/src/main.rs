mod command;
mod tex;

use std::error::Error;
use std::io;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn main() {
    if let Err(error) = run() {
        eprintln!("xtask: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [command] if command == "tex" => tex::verify_all(),
        [command, case] if command == "tex" && case == "common" => tex::verify_common(),
        [command, case] if command == "tex" && case == "convergence" => tex::verify_convergence(),
        [command, case, selected @ ..] if command == "tex" && case == "corpus" => {
            tex::verify_corpus(selected)
        }
        _ => Err(message(
            "usage:\n  cargo xtask tex\n  cargo xtask tex common\n  cargo xtask tex convergence\n  cargo xtask tex corpus [CASE...]",
        )),
    }
}

fn message(text: impl Into<String>) -> Box<dyn Error> {
    Box::new(io::Error::other(text.into()))
}
