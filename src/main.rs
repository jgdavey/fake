use std::env;
use std::io::{self, BufRead};
use std::io::prelude::*;
use std::path::Path;

mod markov;

fn read_line(prompt: &str) -> io::Result<String> {
    let stdout = io::stdout();
    print!("{}", prompt);
    stdout.lock().flush()?;
    let stdin = io::stdin();
    let line1 = stdin.lock().lines().next().unwrap()?;
    Ok(line1)
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let default_file = String::from("example.txt");
    let filename = match args.len() {
        0...1 => &default_file,
        _ => &args[1],
    };

    let mut index = markov::Chain::new();

    index.feed_file(Path::new(filename)).unwrap();

    index.printsizes();

    if filename == &default_file {
        println!("{:#?}", index.nexts);
    }

    loop {
        let res = read_line("seed> ");
        match res {
            Ok(ref seed) if seed == "" => {
                println!("(Empty)");
                println!("\n{}\n", index.generate_str());
            }
            Ok(seed) => {
                println!("{}", seed);
                if let Some(gen) = index.generate_from_best(seed, 140) {
                    println!("\n{}\n", gen)
                }
            },
            Err(e) => println!("Error: {:?}", e)
        }
    }
}
