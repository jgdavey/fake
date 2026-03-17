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

---

## Option A: External sort + memory-mapped flat file

### Build phase

Instead of accumulating in HashMaps, write fixed-width raw records to append-only log files during `feed`:

```
paths.log   — 9 bytes per record: [tok1: u32][tok2: u32][dir: u8][value: u32]
entries.log — 8 bytes per record: [tok1: u32][tok2: u32]
```

No in-memory accumulation of paths/entries. Only the Dict and the current line being tokenized are held in memory.

**Finalize step:**

1. External sort `paths.log` by `(tok1, tok2, dir)`. Fixed-width records make this straightforward: read chunks up to a configurable memory budget, sort each, write to temp files, k-way merge.
2. Stream the sorted records, grouping by `(tok1, tok2)`. Accumulate the current key's values into two `BufferTokSet`s in memory (bounded by occurrences of that one bigram — fine). When the key changes, write the `NextTokens` record to the data region and record its offset in the index. Move to the next key.
3. Same process for `entries.log`.
4. Write final `chain.bin` with sorted index tables pointing into the data region. Serialize Dict to `dict.bin`.

Peak memory during finalize: sort chunk budget + one key's worth of records. Never the full dataset.

**Scale analysis at target corpus size:**

Each 4-gram produces one 9-byte paths record and one 8-byte entries record. At 1 GB of text (~167 M words):
- `paths.log` ≈ 1.5 GB
- `entries.log` ≈ 1.3 GB
- Scratch space during sort ≈ 2–3× the log size (original + sorted runs + merged output)

At this scale a **single-level merge** is sufficient — no recursive multi-pass needed. With a 256 MB sort budget, the 1.5 GB paths log produces ~6 sorted runs, which are merged in one pass. A 256 MB default is a reasonable configuration. Scratch space peaks at roughly 3–4 GB for a 1 GB corpus, which should be documented so the user can ensure sufficient disk headroom.

### chain.bin layout

```
[magic + version header]
[num_paths: u32]
[num_entries: u32]

── paths index (sorted by (tok1, tok2)) ──────────────────────────────
[(tok1: u32, tok2: u32, data_offset: u32) × num_paths]

── entries index (sorted by tok1) ────────────────────────────────────
[(tok: u32, data_offset: u32) × num_entries]

── data region ───────────────────────────────────────────────────────
NextTokens records (at offsets referenced by paths index):
  [fwd_n: u32][(tok: u32, count: u32) × fwd_n]
  [rev_n: u32][(tok: u32, count: u32) × rev_n]

Entry records (at offsets referenced by entries index):
  [n: u32][(tok: u32, count: u32) × n]
```

Fixed-width `u32` pairs throughout — no variable-width encoding. `BufferTokSet`'s 1/2/3-byte packing is an in-memory RAM optimization that is not needed on disk.

### Query phase

Load Dict fully into memory. Memory-map `chain.bin` with `memmap2`. Lookups use binary search on the sorted index arrays into the mmap — essentially in-process with OS-managed paging. `NextTokensView` and `BufferTokSetView` are thin wrappers over `&[u8]` slices into the mmap, so `choose()` works without heap allocation.

### Code structure

```rust
struct ChainBuilder {
    dict: Dict,
    paths_log: BufWriter<File>,
    entries_log: BufWriter<File>,
    sort_budget: usize,           // max bytes to use per sort chunk
}

impl ChainBuilder {
    fn feed_str(&mut self, ...) { /* tokenize → write raw records */ }
    fn finalize(self, out: &Path) -> io::Result<DiskChain> {
        // external sort both logs
        // streaming merge → write chain.bin
        // serialize dict → dict.bin
    }
}

struct DiskChain {
    dict: Dict,     // fully in memory
    mmap: Mmap,     // chain.bin memory-mapped
    // typed slices into mmap for the two sorted indexes
}
```

### Future work: updateability

The sorted flat file is immutable after finalization, but two strategies can add updateability without abandoning the format:

**Append-only segments.** New data goes through the same build pipeline and produces an additional segment file (`chain.2.bin`, etc.) in the same format. Queries look up the key in all segments and sum the counts before choosing. Compaction merges all segments back into one by running the streaming merge step across multiple sorted inputs — the same k-way merge already needed for the external sort, just with more input streams. This is the preferred approach: the build pipeline is fully reused for compaction, count-merging across segments is simple addition, and query overhead is negligible while the segment count stays small (2–3 between compactions).

**Full rebuild.** Re-feed all original data plus new data and write a fresh `chain.bin`, atomically replacing the old one via rename. Simple and correct; suitable when updates are infrequent and the corpus growth is modest.

---

## Option B: redb embedded database

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

---

## Open questions

1. **Temp file location.** The external sort (Option A) needs scratch space of roughly 3–4× the log size during finalization (up to ~4 GB at the 1 GB corpus ceiling). Temp files will be written to a subfolder of the output directory (e.g. `<out>/.tmp/`) and cleaned up after finalization.
