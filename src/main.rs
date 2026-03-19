use std::convert::Infallible;
use std::io::prelude::*;
use std::io::{self, BufRead, Error};
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use warp::Filter;

mod disk;
mod markov;

fn read_line(prompt: &str) -> io::Result<String> {
    let stdout = io::stdout();
    print!("{}", prompt);
    stdout.lock().flush()?;
    let stdin = io::stdin();
    let line = stdin.lock().lines().next();
    line.unwrap_or_else(|| Err(Error::other("EOF")))
}

/// Generate text with Markov chains
#[derive(Parser, Debug)]
#[command(version, about, name = "fake")]
struct Cli {
    /// Activate debug mode
    #[arg(short, long, global = true)]
    debug: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Build a disk index from a corpus file
    Index {
        /// Text corpus file to index
        corpus: PathBuf,
        /// Output directory for index files
        out_dir: PathBuf,
    },
    /// Serve generation queries from a pre-built index
    Serve {
        /// Directory containing index files (chain.bin, dict.bin)
        index_dir: PathBuf,
        /// Run HTTP server on this port (omit for interactive REPL)
        #[arg(short, long)]
        port: Option<u16>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct MarkovRequest {
    seed: Option<String>,
    target: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
struct MarkovResponse {
    response: Option<String>,
}

async fn handle_request(
    req: MarkovRequest,
    chain: Arc<disk::DiskChain>,
) -> Result<impl warp::Reply, Infallible> {
    let target = req.target.unwrap_or(20);
    let response = match req.seed {
        None => chain.generate_best(target),
        Some(ref seed) => chain.generate_best_from(seed, target),
    };
    Ok(warp::reply::json(&MarkovResponse { response }))
}

async fn repl(chain: Arc<disk::DiskChain>) {
    loop {
        let res = read_line("seed> ");
        match res {
            Ok(seed) => {
                println!("{}", seed);
                let response = if seed.is_empty() {
                    chain.generate_best(20)
                } else {
                    chain.generate_best_from(&seed, 20)
                };
                if let Some(generated) = response {
                    println!("\n{}\n", generated);
                }
            }
            Err(_) => {
                println!("\nBye!");
                break;
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Index { corpus, out_dir } => {
            if cli.debug {
                eprintln!("Indexing {} → {}", corpus.display(), out_dir.display());
            }
            let mut builder = disk::ChainBuilder::new(&out_dir).unwrap_or_else(|e| {
                eprintln!("Error creating index builder: {e}");
                std::process::exit(1);
            });
            builder.feed_file(&corpus).unwrap_or_else(|e| {
                eprintln!("Error reading corpus: {e}");
                std::process::exit(1);
            });
            builder.finalize().unwrap_or_else(|e| {
                eprintln!("Error building index: {e}");
                std::process::exit(1);
            });
        }

        Commands::Serve { index_dir, port } => {
            if cli.debug {
                eprintln!("Opening index at {}", index_dir.display());
            }
            let chain = Arc::new(disk::DiskChain::open(&index_dir).unwrap_or_else(|e| {
                eprintln!("Error opening index: {e}");
                std::process::exit(1);
            }));

            if let Some(port) = port {
                // POST / {"seed": "word", "target": 20}
                let chain_filter = warp::any().map(move || Arc::clone(&chain));
                let endpoint = warp::post()
                    .and(warp::body::json())
                    .and(chain_filter)
                    .and_then(handle_request);

                if cli.debug {
                    eprintln!("Binding server on port {port}");
                }
                warp::serve(endpoint).run(([127, 0, 0, 1], port)).await;
            } else {
                repl(chain).await;
            }
        }
    }
}
