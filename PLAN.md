# Implementation Plan: External Sort + Memory-Mapped Index

See [DISK.md](DISK.md) for the full option comparison and rationale for this choice.

## Process model

Two separate CLI subcommands:

```
fake index <corpus-file> <output-dir>   # build the index, then exit
fake serve <output-dir>                 # open index, serve queries immediately
```

`fake serve` must be available almost immediately at startup — it memory-maps `chain.bin` and loads `dict.bin` into memory, then begins serving. No indexing or warmup scan occurs at serve time.

---

## File formats

### dict.bin

All multi-byte integers little-endian. The dict is small enough to load fully into memory at serve time.

```
[magic: b"FAKEDICT"]          8 bytes
[version: u32 LE]             4 bytes
[num_entries: u32 LE]         4 bytes
── entries, in TokID order ───────────────────────────────────────────
[tokid: u32 LE]               4 bytes  (explicit, for validation)
[len: u16 LE]                 2 bytes
[utf8: u8 × len]              variable
```

TokID 0 is always the empty string `""` (the sentinel used as a sequence boundary).

### chain.bin

All multi-byte integers little-endian. Naturally 4-byte aligned throughout — no padding needed. The header includes a byte-order sentinel so a reader can detect a mismatch and fail fast.

```
[magic: b"FAKECHNN"]         8 bytes
[version: u32 LE]            4 bytes
[byte_order: u32 = 0x01]     4 bytes  — reads as 0x01000000 on a big-endian machine; reject if so
[num_paths: u32 LE]          4 bytes
[num_entries: u32 LE]        4 bytes

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

Fixed-width `u32` pairs throughout — no variable-width encoding. `BufferTokSet`'s 1/2/3-byte packing is an in-memory RAM optimization not needed on disk.

---

## Build/Index phase

### Feed step

During `fake index`, write fixed-width raw records to append-only log files instead of accumulating in HashMaps. Only the Dict and the current line being tokenized are held in memory.

```
paths.log   — 13 bytes per record: [tok1: u32][tok2: u32][dir: u8][value: u32]
entries.log —  8 bytes per record: [tok1: u32][tok2: u32]
```

### Finalize step

1. External sort `paths.log` by `(tok1, tok2, dir)`. Read chunks up to the sort budget, sort each, write to temp files under `<output-dir>/.tmp/`, k-way merge.
2. Stream sorted records, grouping by `(tok1, tok2)`. Accumulate the current key's forward and reverse values into two count sets in memory (bounded by occurrences of that one bigram). When the key changes, write the `NextTokens` record to the data region and record its offset in the index. Move to the next key.
3. Same process for `entries.log`, grouped by `tok1`.
4. Write `chain.bin` with the two sorted index arrays followed by the data region.
5. Write `dict.bin`. Rename both into place atomically (see open questions).
6. Clean up `.tmp/`.

Peak memory: sort chunk budget + one key's worth of count pairs. Never the full dataset.

### Scale analysis

Each 4-gram produces one 13-byte paths record and one 8-byte entries record. At 1 GB of text (~167 M words):

- `paths.log` ≈ 2.2 GB
- `entries.log` ≈ 1.3 GB
- Scratch space during sort ≈ 2–3× log size (original + sorted runs + merged output)

A **single-level merge** is sufficient at this scale — no recursive multi-pass needed. With a 256 MB sort budget, the 2.2 GB paths log produces ~9 sorted runs merged in one pass. Scratch space peaks at roughly 5–7 GB for a 1 GB corpus; this should be documented at runtime so the user knows the disk headroom required.

---

## Query phase

Load `dict.bin` fully into memory. Memory-map `chain.bin` with `memmap2`. Lookups binary-search the sorted index arrays into the mmap — essentially in-process with OS-managed paging.

```rust
struct ChainBuilder {
    dict: Dict,
    paths_log: BufWriter<File>,
    entries_log: BufWriter<File>,
    sort_budget: usize,           // max bytes per sort chunk
}

impl ChainBuilder {
    fn feed_str(&mut self, ...) { /* tokenize → write raw records */ }
    fn finalize(self, out: &Path) -> io::Result<DiskChain> {
        // external sort both logs
        // streaming merge → write chain.bin
        // write dict.bin
        // atomic rename into place
    }
}

struct DiskChain {
    dict: Dict,     // fully in memory
    mmap: Mmap,     // chain.bin memory-mapped
    // typed slices into mmap for the two sorted indexes
}
```

`NextTokensView` and `BufferTokSetView` are thin wrappers over `&[u8]` slices into the mmap so that `choose()` works without heap allocation.

---

## Future work: updateability

**Append-only segments.** New data goes through the same build pipeline and produces an additional segment file (`chain.2.bin`, etc.) in the same format. Queries look up the key in all segments and sum the counts before choosing. Compaction merges all segments by running the streaming merge step across multiple sorted inputs — the same k-way merge already needed for the external sort. This is the preferred approach: the build pipeline is fully reused for compaction, count-merging across segments is simple addition, and query overhead is negligible while the segment count stays small (2–3 between compactions).

**Full rebuild.** Re-feed all original data plus new data and write a fresh `chain.bin`, atomically replacing the old one via rename. Suitable when updates are infrequent and corpus growth is modest.

---

## Constraints and decisions

1. **Temp file location.** Temp files during finalization are written to `<output-dir>/.tmp/` and cleaned up on completion. Scratch space peaks at roughly 5–7 GB for a 1 GB corpus; this should be communicated at runtime.

2. **Atomic index replacement.** `dict.bin` and `chain.bin` must always be consistent — TokIDs embedded in `chain.bin` are only valid alongside the `dict.bin` from the same run. `fake index` writes both files into `<output-dir>/.tmp/`, then does a single directory rename to replace the output directory atomically. `fake serve` will continue serving from the old mmap until restarted.

3. **Incomplete index detection.** `fake index` writes a magic footer `b"FAKEEND\n"` (8 bytes) as the very last operation before the rename. `fake serve` checks for this footer at open time and rejects the file immediately if it is absent, rather than serving garbage from a partial write.

4. **4-byte alignment.** All sections of `chain.bin` are naturally 4-byte aligned: the header is 24 bytes, index entries are 12 and 8 bytes, and data records consist entirely of `u32` fields whose sizes are always multiples of 4. Any future additions to the format must preserve this property.
