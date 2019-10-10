use std::env;
use std::io::{self, BufRead, Error, ErrorKind};
use std::io::prelude::*;
use std::path::Path;

mod markov;

fn read_line(prompt: &str) -> io::Result<String> {
    let stdout = io::stdout();
    print!("{}", prompt);
    stdout.lock().flush()?;
    let stdin = io::stdin();
    let line = stdin.lock().lines().next();
    line.unwrap_or_else(|| Err(Error::new(ErrorKind::Other, "EOF")))
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let default_file = String::from("example.txt");
    let filename = match args.len() {
        0..=1 => &default_file,
        _ => &args[1],
    };

    let mut index = markov::Chain::new();

    index.feed_file(Path::new(filename)).unwrap();

    index.printsizes();

    loop {
        let res = read_line("seed> ");
        match res {
            Ok(ref seed) if seed == "" => {
                println!("(Empty)");
                if let Some(gen) = index.generate_best(140) {
                    println!("\n{}\n", gen);
                }
            },
            Ok(seed) => {
                println!("{}", seed);
                if let Some(gen) = index.generate_best_from(seed, 140) {
                    println!("\n{}\n", gen)
                }
            },
            Err(_) => {
                println!("\nBye!");
                break
            }
        }
    }
}
