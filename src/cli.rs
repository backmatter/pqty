use clap::Parser;

mod args;
mod command;
mod config;
pub(crate) mod protocol;

use args::Cli;
use command::run;

pub fn main_entry() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("pqty: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests;
