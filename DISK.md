# Disk-backed Index Design

## Process model

Indexing and querying are **two separate processes** with separate CLI subcommands:

```
fake index <corpus-file> <output-dir>   # build the index, then exit
fake serve <output-dir>                 # open index, serve queries immediately
```

The query process (`serve`) must be **available almost immediately at startup** — it must not do any indexing or heavy computation before it can respond to requests. It opens the index on disk and begins serving. Indexing (`index`) is a separate offline step that may take as long as needed.

This means the on-disk format must support near-instant open: memory-mapping a file or opening a database handle must be sufficient to begin serving, with no warmup scan or in-memory rebuild.

## Constraints

- **Build-once, query-many** model is acceptable (no online updates required).
- Even during indexing, the full data model must never need to fit in RAM.
- **Corpus size: 100 MB – 1 GB** of text input. At ~6 bytes/word, that is roughly 17–167 million words.
- The **Dict** (string interner) can stay in memory throughout — vocabulary size is bounded regardless of corpus size (even billion-word corpora have a few million unique words, ~tens of MB).
- **TokenPaths** and **Entries** are the bulk of the data and must be disk-backed.

---

## Current in-memory structure (what's being replaced)

- **`Dict`** — bidirectional `str ↔ Symbol` interner. Used during feed (writes) and output (reads by Symbol).
- **`TokenPaths`** — `HashMap<TokID, HashMap<TokID, NextTokens>>` where `NextTokens` holds two `BufferTokSet`s (forward + reverse successors). This is the bulk of the data.
- **`Entries`** — `HashMap<TokID, BufferTokSet>`. Used only for 1-word seeds; lookup by single TokID.

The chosen implementation approach is documented in [PLAN.md](PLAN.md).

---

## Alternative that was considered: redb embedded database

`redb` is a pure-Rust embedded key-value store with typed tables, explicit transactions, and a single-file database. It is actively maintained and stable.

### Key and value encoding

On disk, token sets are stored as **count-encoded pairs** rather than flat repeated occurrences. `BufferTokSet`'s variable-width byte packing (1/2/3 bytes per token ID depending on magnitude) is an in-memory space optimization that is unnecessary on disk — fixed-width `u32` fields are simpler and fast enough with OS-level caching.

```rust
// paths table key: [u8; 8], big-endian for correct sort order
fn paths_key(tok1: TokID, tok2: TokID) -> [u8; 8] { ... }

// paths table value — each direction is a flat array of fixed-width pairs:
// [fwd_n: u32][(tok: u32, count: u32) × fwd_n]
// [rev_n: u32][(tok: u32, count: u32) × rev_n]

// entries table value:
// [n: u32][(tok: u32, count: u32) × n]

type CountSet = HashMap<TokID, u32>;

const PATHS_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("paths");
const ENTRIES_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("entries");
```

### Build phase

redb uses explicit write transactions. To bound memory, commit a transaction every N lines (e.g. every 10 000 lines) rather than opening one transaction for the entire corpus.

```rust
pub struct ChainBuilder {
    dict: Dict,
    db: redb::Database,
    pending: u32,         // lines since last commit
}

fn append_paths(&self, wtxn: &WriteTransaction, tok1: TokID, tok2: TokID, fwd: TokID, rev: TokID) -> redb::Result<()> {
    let mut table = wtxn.open_table(PATHS_TABLE)?;
    let key = paths_key(tok1, tok2);
    let (mut f, mut r) = table.get(&key[..])?
        .map(|v| decode_paths(v.value()))
        .unwrap_or_default();
    *f.entry(fwd).or_insert(0) += 1;
    *r.entry(rev).or_insert(0) += 1;
    table.insert(&key[..], &encode_paths(&f, &r))?;
    Ok(())
}
```

`feed_str` tokenizes, maps words through the Dict, calls `append_paths` for each 4-gram window, and periodically commits. redb pages its B-tree to disk automatically — memory stays bounded by its internal cache and the transaction batch size.

No separate finalization step. The redb file is the index for both build and query.

### Query phase

```rust
pub struct DiskChain {
    dict: Dict,           // loaded from dict.bin at open time
    db: redb::Database,
}

fn get_paths(&self, tok1: TokID, tok2: TokID) -> redb::Result<Option<(CountSet, CountSet)>> {
    let rtxn = self.db.begin_read()?;
    let table = rtxn.open_table(PATHS_TABLE)?;
    let key = paths_key(tok1, tok2);
    Ok(table.get(&key[..])?.map(|v| decode_paths(v.value())))
}
```

The token iterator opens a read transaction once per generation call and calls `get_paths` on each step — one redb lookup per generated token rather than a pure memory access.

Weighted choice from a `CountSet`:

```rust
fn choose_from(set: &CountSet, rng: &mut ThreadRng) -> TokID {
    let pairs: Vec<_> = set.iter().collect();
    *pairs.choose_weighted(rng, |e| *e.1).unwrap().0
}
```

---

## Tradeoffs

| | Option A: external sort + mmap | Option B: redb |
|---|---|---|
| Build complexity | High — external sort, k-way merge, offset tracking | Low — read-modify-write per 4-gram, periodic commit |
| Build speed | Faster — sequential appends then one streaming pass | Slow — up to 167 M random-access read+write round-trips per 4-gram at 1 GB corpus |
| Query speed | Fast — binary search into mmap, zero alloc per hop | Slower — redb lookup + deserialize per hop |
| Memory during build | Bounded by configurable sort chunk size | Bounded by redb's cache + transaction batch size |
| Final index size | Compact — exactly what you write | Larger — B-tree overhead |
| Index is portable | Yes — a pair of plain binary files | Yes — a single file |
| Serve startup time | Instant — mmap + load dict only | Fast — redb open + WAL recovery (sub-second for healthy db) |
| Updateable later | No — rebuild required | Yes |
| Dependency risk | Only `memmap2` (stable, tiny, widely used) | redb stable and actively maintained |

### Recommendation

- Choose **Option A** if generation speed matters (many requests, low latency), you want the most compact output, or you want the fastest possible serve startup. It also aligns most naturally with the two-process model since `fake serve` is reduced to `mmap(chain.bin) + load(dict.bin)`.
- Choose **Option B** if build simplicity is the priority, or if you anticipate needing to append to the index later without a full rebuild. Serve startup is still fast but involves redb's initialization path.
