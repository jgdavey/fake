use std::io::prelude::*;
use std::io::{self, BufRead, Error, ErrorKind};
use std::path::PathBuf;

use structopt::StructOpt;

mod markov;

fn read_line(prompt: &str) -> io::Result<String> {
    let stdout = io::stdout();
    print!("{}", prompt);
    stdout.lock().flush()?;
    let stdin = io::stdin();
    let line = stdin.lock().lines().next();
    line.unwrap_or_else(|| Err(Error::new(ErrorKind::Other, "EOF")))
}

/// Generate text with markov chains
#[derive(StructOpt, Debug)]
#[structopt(name = "fake")]
struct Config {
    /// Activate debug mode
    #[structopt(short, long)]
    debug: bool,

    /// File to process
    #[structopt(name = "INPUT", parse(from_os_str))]
    input: PathBuf,
}

fn setup_index(config: &Config) -> markov::Chain {
    let mut index = markov::Chain::new();
    index.feed_file(&config.input).unwrap();
    index
}

fn repl(mut index: markov::Chain) {
    loop {
        let res = read_line("seed> ");
        match res {
            Ok(ref seed) if seed == "" => {
                println!("(Empty)");
                if let Some(gen) = index.generate_best(100) {
                    println!("\n{}\n", gen);
                }
            }
            Ok(seed) => {
                println!("{}", seed);
                if let Some(gen) = index.generate_best_from(seed, 100) {
                    println!("\n{}\n", gen)
                }
            }
            Err(_) => {
                println!("\nBye!");
                break;
            }
        }
    }
}

fn main() {
    let config = Config::from_args();
    if config.debug {
        println!("Config: {:?}", config);
    }

    let index = setup_index(&config);

    if config.debug {
        index.printsizes();
    }

    repl(index);
}
