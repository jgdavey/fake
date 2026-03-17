# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build          # build debug binary
cargo build --release
cargo test           # run all tests
cargo clippy         # lint
cargo run -- <file>  # run with a text corpus
```

Run a single test by name:
```bash
cargo test markov::tests::small_values
```

## Architecture

**fake** is a Markov chain text generator. The two source files are:

### `src/markov.rs` — Core logic

- **`Dict`** — String interner mapping tokens (words) to integer symbols, backed by the `strena` crate.
- **`BufferTokSet`** — Variable-length byte-packed storage for token frequency data; uses 1/2/3 bytes per value depending on magnitude to minimize memory.
- **`TokenPaths`** — Nested `HashMap` storing forward and reverse token sequences (the actual Markov graph).
- **`Chain`** — Top-level struct combining `Dict`, `TokenPaths`, and an `entries` map. Exposed API: `feed_str`, `feed_file`, `generate_best`, `generate_best_from`.

Generation is **bidirectional**: from a seed word, the chain walks backwards to a start token, then forwards to an end token, then merges. `generate_best`/`generate_best_from` run 50 random walks and return the one closest to a target word count.

### `src/main.rs` — CLI and server

Two runtime modes, selected by whether `--port` is passed:

1. **REPL mode**: reads seeds interactively from stdin, prints generated text.
2. **Server mode**: runs a `warp` HTTP server. `POST /` accepts `{"seed": "...", "target": 20}` JSON and returns `{"response": "..."}`.

Both modes use a shared `tokio` async architecture with `mpsc` channels — a dedicated task owns the `Chain` and responds to generation requests from either the REPL loop or the HTTP handler.
