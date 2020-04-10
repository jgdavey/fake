use std::convert::Infallible;
use std::io::prelude::*;
use std::io::{self, BufRead, Error, ErrorKind};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use structopt::StructOpt;
use tokio::sync::mpsc;
use warp::Filter;

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

    /// Activate debug mode
    #[structopt(short, long)]
    port: Option<u16>,

    /// File to process
    #[structopt(name = "INPUT", parse(from_os_str))]
    input: PathBuf,
}

fn setup_index(config: &Config) -> markov::Chain {
    let mut index = markov::Chain::new();
    index.feed_file(&config.input).unwrap();
    index
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

type MarkovRequestMessage = (MarkovRequest, mpsc::Sender<MarkovResponse>);

async fn respond(
    req: MarkovRequest,
    mut tx_req: mpsc::Sender<MarkovRequestMessage>,
) -> Result<MarkovResponse, Infallible> {
    let (tx_resp, mut rx_resp) = mpsc::channel::<MarkovResponse>(1);
    tx_req.send((req, tx_resp)).await.expect("Oh noes");
    let resp = rx_resp.recv().await;
    Ok(resp.unwrap())
}

async fn to_json(resp: MarkovResponse) -> Result<impl warp::Reply, Infallible> {
    Ok(warp::reply::json(&resp))
}

async fn repl(tx_req: mpsc::Sender<MarkovRequestMessage>) {
    loop {
        let res = read_line("seed> ");
        match res {
            Ok(seed) => {
                println!("{}", seed);
                let seedlet = match seed.as_str() {
                    "" => None,
                    _ => Some(seed),
                };
                let input = MarkovRequest {
                    seed: seedlet,
                    target: None,
                };
                if let Ok(resp) = respond(input, tx_req.clone()).await {
                    if let Some(gen) = resp.response {
                        println!("\n{}\n", gen);
                    }
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
    let config = Config::from_args();
    if config.debug {
        println!("Config: {:?}", config);
        println!("Indexing {}...", config.input.display());
    }

    let mut index = setup_index(&config);

    if config.debug {
        index.printsizes();
    }
    let debug = config.debug;

    let (tx_req, mut rx_req) = mpsc::channel::<MarkovRequestMessage>(100);

    let _responder = tokio::spawn(async move {
        while let Some(work) = rx_req.recv().await {
            let (req, mut tx_resp): MarkovRequestMessage = work;
            let target = req.target.unwrap_or(20);
            if debug {
                println!("Processing request: {:?}", req);
            }
            let response = match req.seed {
                None => index.generate_best(target),
                Some(seed) => index.generate_best_from(seed, target),
            };
            tx_resp
                .send(MarkovResponse { response })
                .await
                .expect("what?");
        }
    });

    if let Some(port) = config.port {
        // POST / {"seed": "Sean", "target": 20}
        let endpoint = warp::post()
            .and(warp::body::json())
            .and(warp::any().map(move || tx_req.clone()))
            .and_then(respond)
            .and_then(to_json);

        if config.debug {
            println!("Binding server on port {}", port);
        }

        warp::serve(endpoint).run(([127, 0, 0, 1], port)).await;
    } else {
        repl(tx_req).await;
    }
}
