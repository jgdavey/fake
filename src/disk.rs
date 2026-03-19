use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use memmap2::Mmap;
use rand::rngs::ThreadRng;
use rand::{RngExt, rng};

use crate::markov::{Dict, Direction};

// ──────────────────────────────────────────────────────────────────────────────
// Log record formats
// ──────────────────────────────────────────────────────────────────────────────

const PATHS_RECORD_LEN: usize = 13; // tok1(4) tok2(4) dir(1) value(4)
const ENTRIES_RECORD_LEN: usize = 8; // tok1(4) tok2(4)

struct PathsRec {
    tok1: u32,
    tok2: u32,
    dir: u8,
    value: u32,
}

impl PathsRec {
    fn sort_key(&self) -> (u32, u32, u8) {
        (self.tok1, self.tok2, self.dir)
    }

    fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(&self.tok1.to_le_bytes())?;
        w.write_all(&self.tok2.to_le_bytes())?;
        w.write_all(&[self.dir])?;
        w.write_all(&self.value.to_le_bytes())
    }

    fn read_from(buf: &[u8]) -> Self {
        let tok1 = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let tok2 = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        let dir = buf[8];
        let value = u32::from_le_bytes(buf[9..13].try_into().unwrap());
        PathsRec { tok1, tok2, dir, value }
    }
}

struct EntriesRec {
    tok1: u32,
    tok2: u32,
}

impl EntriesRec {
    fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(&self.tok1.to_le_bytes())?;
        w.write_all(&self.tok2.to_le_bytes())
    }

    fn read_from(buf: &[u8]) -> Self {
        let tok1 = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let tok2 = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        EntriesRec { tok1, tok2 }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// External sort
// ──────────────────────────────────────────────────────────────────────────────

fn external_sort_paths(log_path: &Path, tmp_dir: &Path, budget: usize) -> io::Result<PathBuf> {
    let records_per_chunk = (budget / PATHS_RECORD_LEN).max(1);
    let mut chunk_files: Vec<PathBuf> = Vec::new();
    {
        let mut f = BufReader::new(File::open(log_path)?);
        let mut chunk: Vec<PathsRec> = Vec::with_capacity(records_per_chunk);
        let mut buf = [0u8; PATHS_RECORD_LEN];
        let mut chunk_idx = 0usize;
        loop {
            match f.read_exact(&mut buf) {
                Ok(()) => {
                    chunk.push(PathsRec::read_from(&buf));
                    if chunk.len() >= records_per_chunk {
                        let out = tmp_dir.join(format!("paths_chunk_{chunk_idx}.tmp"));
                        write_sorted_paths_chunk(&mut chunk, &out)?;
                        chunk_files.push(out);
                        chunk_idx += 1;
                        chunk.clear();
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }
        }
        if !chunk.is_empty() {
            let out = tmp_dir.join(format!("paths_chunk_{chunk_idx}.tmp"));
            write_sorted_paths_chunk(&mut chunk, &out)?;
            chunk_files.push(out);
        }
    }

    let sorted_out = tmp_dir.join("paths_sorted.tmp");
    if chunk_files.is_empty() {
        File::create(&sorted_out)?;
    } else if chunk_files.len() == 1 {
        fs::rename(&chunk_files[0], &sorted_out)?;
    } else {
        kway_merge_paths(&chunk_files, &sorted_out)?;
        for f in &chunk_files {
            let _ = fs::remove_file(f);
        }
    }
    Ok(sorted_out)
}

fn write_sorted_paths_chunk(chunk: &mut Vec<PathsRec>, out: &Path) -> io::Result<()> {
    chunk.sort_unstable_by_key(|r| r.sort_key());
    let mut w = BufWriter::new(File::create(out)?);
    for r in chunk.iter() {
        r.write_to(&mut w)?;
    }
    Ok(())
}

struct PathsHeapEntry {
    rec: PathsRec,
    file_idx: usize,
}

impl PartialEq for PathsHeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.rec.sort_key() == other.rec.sort_key()
    }
}
impl Eq for PathsHeapEntry {}
impl PartialOrd for PathsHeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for PathsHeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse for min-heap behavior
        other.rec.sort_key().cmp(&self.rec.sort_key())
    }
}

fn kway_merge_paths(chunk_files: &[PathBuf], out: &Path) -> io::Result<()> {
    let mut readers: Vec<BufReader<File>> = chunk_files
        .iter()
        .map(|p| Ok(BufReader::new(File::open(p)?)))
        .collect::<io::Result<_>>()?;

    let mut heap: BinaryHeap<PathsHeapEntry> = BinaryHeap::new();
    let mut buf = [0u8; PATHS_RECORD_LEN];
    for (i, r) in readers.iter_mut().enumerate() {
        if r.read_exact(&mut buf).is_ok() {
            heap.push(PathsHeapEntry { rec: PathsRec::read_from(&buf), file_idx: i });
        }
    }

    let mut w = BufWriter::new(File::create(out)?);
    while let Some(entry) = heap.pop() {
        entry.rec.write_to(&mut w)?;
        let i = entry.file_idx;
        if readers[i].read_exact(&mut buf).is_ok() {
            heap.push(PathsHeapEntry { rec: PathsRec::read_from(&buf), file_idx: i });
        }
    }
    Ok(())
}

fn external_sort_entries(log_path: &Path, tmp_dir: &Path, budget: usize) -> io::Result<PathBuf> {
    let records_per_chunk = (budget / ENTRIES_RECORD_LEN).max(1);
    let mut chunk_files: Vec<PathBuf> = Vec::new();
    {
        let mut f = BufReader::new(File::open(log_path)?);
        let mut chunk: Vec<EntriesRec> = Vec::with_capacity(records_per_chunk);
        let mut buf = [0u8; ENTRIES_RECORD_LEN];
        let mut chunk_idx = 0usize;
        loop {
            match f.read_exact(&mut buf) {
                Ok(()) => {
                    chunk.push(EntriesRec::read_from(&buf));
                    if chunk.len() >= records_per_chunk {
                        let out = tmp_dir.join(format!("entries_chunk_{chunk_idx}.tmp"));
                        write_sorted_entries_chunk(&mut chunk, &out)?;
                        chunk_files.push(out);
                        chunk_idx += 1;
                        chunk.clear();
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }
        }
        if !chunk.is_empty() {
            let out = tmp_dir.join(format!("entries_chunk_{chunk_idx}.tmp"));
            write_sorted_entries_chunk(&mut chunk, &out)?;
            chunk_files.push(out);
        }
    }

    let sorted_out = tmp_dir.join("entries_sorted.tmp");
    if chunk_files.is_empty() {
        File::create(&sorted_out)?;
    } else if chunk_files.len() == 1 {
        fs::rename(&chunk_files[0], &sorted_out)?;
    } else {
        kway_merge_entries(&chunk_files, &sorted_out)?;
        for f in &chunk_files {
            let _ = fs::remove_file(f);
        }
    }
    Ok(sorted_out)
}

fn write_sorted_entries_chunk(chunk: &mut Vec<EntriesRec>, out: &Path) -> io::Result<()> {
    chunk.sort_unstable_by_key(|r| (r.tok1, r.tok2));
    let mut w = BufWriter::new(File::create(out)?);
    for r in chunk.iter() {
        r.write_to(&mut w)?;
    }
    Ok(())
}

struct EntriesHeapEntry {
    rec: EntriesRec,
    file_idx: usize,
}

impl PartialEq for EntriesHeapEntry {
    fn eq(&self, other: &Self) -> bool {
        (self.rec.tok1, self.rec.tok2) == (other.rec.tok1, other.rec.tok2)
    }
}
impl Eq for EntriesHeapEntry {}
impl PartialOrd for EntriesHeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for EntriesHeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        (other.rec.tok1, other.rec.tok2).cmp(&(self.rec.tok1, self.rec.tok2))
    }
}

fn kway_merge_entries(chunk_files: &[PathBuf], out: &Path) -> io::Result<()> {
    let mut readers: Vec<BufReader<File>> = chunk_files
        .iter()
        .map(|p| Ok(BufReader::new(File::open(p)?)))
        .collect::<io::Result<_>>()?;

    let mut heap: BinaryHeap<EntriesHeapEntry> = BinaryHeap::new();
    let mut buf = [0u8; ENTRIES_RECORD_LEN];
    for (i, r) in readers.iter_mut().enumerate() {
        if r.read_exact(&mut buf).is_ok() {
            heap.push(EntriesHeapEntry { rec: EntriesRec::read_from(&buf), file_idx: i });
        }
    }

    let mut w = BufWriter::new(File::create(out)?);
    while let Some(entry) = heap.pop() {
        entry.rec.write_to(&mut w)?;
        let i = entry.file_idx;
        if readers[i].read_exact(&mut buf).is_ok() {
            heap.push(EntriesHeapEntry { rec: EntriesRec::read_from(&buf), file_idx: i });
        }
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// ChainBuilder — index phase
// ──────────────────────────────────────────────────────────────────────────────

pub struct ChainBuilder {
    dict: Dict,
    paths_log: BufWriter<File>,
    entries_log: BufWriter<File>,
    sort_budget: usize,
    out_dir: PathBuf,
    tmp_dir: PathBuf,
}

const DEFAULT_SORT_BUDGET: usize = 256 * 1024 * 1024; // 256 MB

impl ChainBuilder {
    pub fn new(out_dir: &Path) -> io::Result<Self> {
        Self::with_sort_budget(out_dir, DEFAULT_SORT_BUDGET)
    }

    pub fn with_sort_budget(out_dir: &Path, sort_budget: usize) -> io::Result<Self> {
        let tmp_dir = out_dir.join(".tmp");
        fs::create_dir_all(&tmp_dir)?;
        let paths_log = BufWriter::new(File::create(tmp_dir.join("paths.log"))?);
        let entries_log = BufWriter::new(File::create(tmp_dir.join("entries.log"))?);
        Ok(ChainBuilder {
            dict: Dict::new(),
            paths_log,
            entries_log,
            sort_budget,
            out_dir: out_dir.to_path_buf(),
            tmp_dir,
        })
    }

    pub fn feed_str(&mut self, s: &str) -> io::Result<()> {
        let words: Vec<&str> = s.split_whitespace().collect();
        if words.is_empty() {
            return Ok(());
        }
        let none = self.dict.tokid("");
        let mut toks: Vec<u32> = vec![none, none, none];
        toks.extend(words.iter().map(|t| self.dict.tokid(t)));
        toks.push(none);
        toks.push(none);
        for p in toks.windows(4) {
            if let [a, b, c, d] = *p {
                // Forward: prefix (b,c) → d
                self.paths_log.write_all(&b.to_le_bytes())?;
                self.paths_log.write_all(&c.to_le_bytes())?;
                self.paths_log.write_all(&[0u8])?;
                self.paths_log.write_all(&d.to_le_bytes())?;
                // Reverse: prefix (b,c) → a
                self.paths_log.write_all(&b.to_le_bytes())?;
                self.paths_log.write_all(&c.to_le_bytes())?;
                self.paths_log.write_all(&[1u8])?;
                self.paths_log.write_all(&a.to_le_bytes())?;
                // Entry: b → c
                self.entries_log.write_all(&b.to_le_bytes())?;
                self.entries_log.write_all(&c.to_le_bytes())?;
            }
        }
        Ok(())
    }

    pub fn feed_file(&mut self, path: &Path) -> io::Result<()> {
        let f = BufReader::new(File::open(path)?);
        for line in f.lines() {
            self.feed_str(&line?)?;
        }
        Ok(())
    }

    pub fn finalize(mut self) -> io::Result<DiskChain> {
        self.paths_log.flush()?;
        self.entries_log.flush()?;
        drop(self.paths_log);
        drop(self.entries_log);

        eprintln!("Sorting paths log...");
        let sorted_paths =
            external_sort_paths(&self.tmp_dir.join("paths.log"), &self.tmp_dir, self.sort_budget)?;

        eprintln!("Sorting entries log...");
        let sorted_entries = external_sort_entries(
            &self.tmp_dir.join("entries.log"),
            &self.tmp_dir,
            self.sort_budget,
        )?;

        eprintln!("Building chain index...");

        let path_data_tmp = self.tmp_dir.join("path_data.tmp");
        let entry_data_tmp = self.tmp_dir.join("entry_data.tmp");

        let mut path_index: Vec<(u32, u32, u32)> = Vec::new(); // (tok1, tok2, rel_offset)
        let mut path_data_size: u32 = 0;

        // Stream sorted paths → build path index and path data
        {
            let mut f = BufReader::new(File::open(&sorted_paths)?);
            let mut w = BufWriter::new(File::create(&path_data_tmp)?);
            let mut buf = [0u8; PATHS_RECORD_LEN];

            let mut current_key: Option<(u32, u32)> = None;
            let mut fwd_counts: HashMap<u32, u32> = HashMap::new();
            let mut rev_counts: HashMap<u32, u32> = HashMap::new();

            loop {
                let got_rec = match f.read_exact(&mut buf) {
                    Ok(()) => Some(PathsRec::read_from(&buf)),
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => None,
                    Err(e) => return Err(e),
                };

                let new_key = got_rec.as_ref().map(|r| (r.tok1, r.tok2));

                if new_key != current_key {
                    if let Some(key) = current_key {
                        path_index.push((key.0, key.1, path_data_size));
                        path_data_size +=
                            write_next_tokens(&mut w, &fwd_counts, &rev_counts)?;
                        fwd_counts.clear();
                        rev_counts.clear();
                    }
                    current_key = new_key;
                }

                match got_rec {
                    Some(rec) => {
                        if rec.dir == 0 {
                            *fwd_counts.entry(rec.value).or_insert(0) += 1;
                        } else {
                            *rev_counts.entry(rec.value).or_insert(0) += 1;
                        }
                    }
                    None => break,
                }
            }
        }

        let mut entry_index: Vec<(u32, u32)> = Vec::new(); // (tok, rel_offset)
        let mut entry_data_size: u32 = 0;

        // Stream sorted entries → build entry index and entry data
        {
            let mut f = BufReader::new(File::open(&sorted_entries)?);
            let mut w = BufWriter::new(File::create(&entry_data_tmp)?);
            let mut buf = [0u8; ENTRIES_RECORD_LEN];

            let mut current_tok: Option<u32> = None;
            let mut counts: HashMap<u32, u32> = HashMap::new();

            loop {
                let got_rec = match f.read_exact(&mut buf) {
                    Ok(()) => Some(EntriesRec::read_from(&buf)),
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => None,
                    Err(e) => return Err(e),
                };

                let new_tok = got_rec.as_ref().map(|r| r.tok1);

                if new_tok != current_tok {
                    if let Some(tok) = current_tok {
                        entry_index.push((tok, entry_data_size));
                        entry_data_size += write_count_pairs(&mut w, &counts)?;
                        counts.clear();
                    }
                    current_tok = new_tok;
                }

                match got_rec {
                    Some(rec) => {
                        *counts.entry(rec.tok2).or_insert(0) += 1;
                    }
                    None => break,
                }
            }
        }

        // Compute layout
        let num_paths = path_index.len() as u32;
        let num_entries = entry_index.len() as u32;
        let header_size: u32 = 24;
        let path_index_size: u32 = num_paths * 12;
        let entry_index_size: u32 = num_entries * 8;
        let data_start: u32 = header_size + path_index_size + entry_index_size;

        // Write chain.bin to tmp
        let chain_bin_tmp = self.tmp_dir.join("chain.bin");
        {
            let mut w = BufWriter::new(File::create(&chain_bin_tmp)?);

            // Header (24 bytes)
            w.write_all(b"FAKECHNN")?;
            w.write_all(&1u32.to_le_bytes())?; // version
            w.write_all(&1u32.to_le_bytes())?; // byte-order sentinel
            w.write_all(&num_paths.to_le_bytes())?;
            w.write_all(&num_entries.to_le_bytes())?;

            // Path index (sorted by (tok1, tok2) — already sorted from streaming)
            for &(tok1, tok2, off) in &path_index {
                w.write_all(&tok1.to_le_bytes())?;
                w.write_all(&tok2.to_le_bytes())?;
                w.write_all(&(data_start + off).to_le_bytes())?;
            }

            // Entry index (sorted by tok — already sorted from streaming)
            for &(tok, off) in &entry_index {
                w.write_all(&tok.to_le_bytes())?;
                w.write_all(&(data_start + path_data_size + off).to_le_bytes())?;
            }

            // Data: path data then entry data
            copy_file_to_writer(&path_data_tmp, &mut w)?;
            copy_file_to_writer(&entry_data_tmp, &mut w)?;

            // Magic footer — written last so serve can detect incomplete index
            w.write_all(b"FAKEEND\n")?;
        }

        // Write dict.bin to tmp
        let dict_bin_tmp = self.tmp_dir.join("dict.bin");
        self.dict.save(&dict_bin_tmp)?;

        // Atomic rename into out_dir
        fs::create_dir_all(&self.out_dir)?;
        fs::rename(&chain_bin_tmp, self.out_dir.join("chain.bin"))?;
        fs::rename(&dict_bin_tmp, self.out_dir.join("dict.bin"))?;

        // Clean up .tmp/
        for name in &[
            "paths.log",
            "entries.log",
            "paths_sorted.tmp",
            "entries_sorted.tmp",
            "path_data.tmp",
            "entry_data.tmp",
        ] {
            let _ = fs::remove_file(self.tmp_dir.join(name));
        }
        let _ = fs::remove_dir(&self.tmp_dir);

        eprintln!("Index written to {}", self.out_dir.display());
        DiskChain::open(&self.out_dir)
    }
}

fn write_next_tokens(
    w: &mut impl Write,
    fwd: &HashMap<u32, u32>,
    rev: &HashMap<u32, u32>,
) -> io::Result<u32> {
    let mut bytes = 0u32;
    w.write_all(&(fwd.len() as u32).to_le_bytes())?;
    bytes += 4;
    for (&tok, &count) in fwd {
        w.write_all(&tok.to_le_bytes())?;
        w.write_all(&count.to_le_bytes())?;
        bytes += 8;
    }
    w.write_all(&(rev.len() as u32).to_le_bytes())?;
    bytes += 4;
    for (&tok, &count) in rev {
        w.write_all(&tok.to_le_bytes())?;
        w.write_all(&count.to_le_bytes())?;
        bytes += 8;
    }
    Ok(bytes)
}

fn write_count_pairs(w: &mut impl Write, counts: &HashMap<u32, u32>) -> io::Result<u32> {
    let mut bytes = 0u32;
    w.write_all(&(counts.len() as u32).to_le_bytes())?;
    bytes += 4;
    for (&tok, &count) in counts {
        w.write_all(&tok.to_le_bytes())?;
        w.write_all(&count.to_le_bytes())?;
        bytes += 8;
    }
    Ok(bytes)
}

fn copy_file_to_writer(src: &Path, dst: &mut impl Write) -> io::Result<()> {
    let mut f = BufReader::new(File::open(src)?);
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n])?;
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// DiskChain — query phase
// ──────────────────────────────────────────────────────────────────────────────

pub struct DiskChain {
    dict: Dict,
    mmap: Mmap,
    num_paths: u32,
    num_entries: u32,
    path_index_off: usize,
    entry_index_off: usize,
}

impl DiskChain {
    pub fn open(dir: &Path) -> io::Result<Self> {
        let chain_path = dir.join("chain.bin");
        let dict_path = dir.join("dict.bin");

        // Validate footer before mapping — detect incomplete index
        {
            let mut f = File::open(&chain_path)?;
            let len = f.seek(SeekFrom::End(0))?;
            if len < 8 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "chain.bin too small"));
            }
            f.seek(SeekFrom::End(-8))?;
            let mut footer = [0u8; 8];
            f.read_exact(&mut footer)?;
            if &footer != b"FAKEEND\n" {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "chain.bin is incomplete (missing footer) — was indexing interrupted?",
                ));
            }
        }

        let dict = Dict::load(&dict_path)?;

        let file = File::open(&chain_path)?;
        // SAFETY: we treat chain.bin as read-only after index is complete.
        let mmap = unsafe { Mmap::map(&file)? };

        if mmap.len() < 24 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "chain.bin header truncated"));
        }
        if &mmap[0..8] != b"FAKECHNN" {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid chain.bin magic"));
        }
        let version = u32::from_le_bytes(mmap[8..12].try_into().unwrap());
        if version != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported chain.bin version",
            ));
        }
        let byte_order = u32::from_le_bytes(mmap[12..16].try_into().unwrap());
        if byte_order != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "chain.bin byte order mismatch — file was built on a big-endian machine",
            ));
        }
        let num_paths = u32::from_le_bytes(mmap[16..20].try_into().unwrap());
        let num_entries = u32::from_le_bytes(mmap[20..24].try_into().unwrap());

        let path_index_off = 24usize;
        let entry_index_off = path_index_off + num_paths as usize * 12;

        Ok(DiskChain { dict, mmap, num_paths, num_entries, path_index_off, entry_index_off })
    }

    /// Binary search the path index for (tok1, tok2). Returns absolute mmap offset into data region.
    fn find_path(&self, tok1: u32, tok2: u32) -> Option<usize> {
        let (mut lo, mut hi) = (0usize, self.num_paths as usize);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let off = self.path_index_off + mid * 12;
            let t1 = u32::from_le_bytes(self.mmap[off..off + 4].try_into().unwrap());
            let t2 = u32::from_le_bytes(self.mmap[off + 4..off + 8].try_into().unwrap());
            match (t1, t2).cmp(&(tok1, tok2)) {
                Ordering::Less => lo = mid + 1,
                Ordering::Greater => hi = mid,
                Ordering::Equal => {
                    let data_off =
                        u32::from_le_bytes(self.mmap[off + 8..off + 12].try_into().unwrap());
                    return Some(data_off as usize);
                }
            }
        }
        None
    }

    /// Binary search the entry index for tok. Returns absolute mmap offset into data region.
    fn find_entry(&self, tok: u32) -> Option<usize> {
        let (mut lo, mut hi) = (0usize, self.num_entries as usize);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let off = self.entry_index_off + mid * 8;
            let t = u32::from_le_bytes(self.mmap[off..off + 4].try_into().unwrap());
            match t.cmp(&tok) {
                Ordering::Less => lo = mid + 1,
                Ordering::Greater => hi = mid,
                Ordering::Equal => {
                    let data_off =
                        u32::from_le_bytes(self.mmap[off + 4..off + 8].try_into().unwrap());
                    return Some(data_off as usize);
                }
            }
        }
        None
    }

    fn read_pairs(&self, off: usize, n: usize) -> Vec<(u32, u32)> {
        let mut pairs = Vec::with_capacity(n);
        for i in 0..n {
            let o = off + i * 8;
            let tok = u32::from_le_bytes(self.mmap[o..o + 4].try_into().unwrap());
            let count = u32::from_le_bytes(self.mmap[o + 4..o + 8].try_into().unwrap());
            pairs.push((tok, count));
        }
        pairs
    }

    fn choose_from_pairs(pairs: &[(u32, u32)], rng: &mut ThreadRng) -> Option<u32> {
        let total: u64 = pairs.iter().map(|(_, c)| *c as u64).sum();
        if total == 0 {
            return None;
        }
        let mut r: u64 = rng.random_range(0..total);
        for &(tok, count) in pairs {
            if r < count as u64 {
                return Some(tok);
            }
            r -= count as u64;
        }
        None
    }

    fn choose_next(
        &self,
        tok1: u32,
        tok2: u32,
        dir: &Direction,
        rng: &mut ThreadRng,
    ) -> Option<u32> {
        let data_off = self.find_path(tok1, tok2)?;
        let fwd_n =
            u32::from_le_bytes(self.mmap[data_off..data_off + 4].try_into().unwrap()) as usize;
        match dir {
            Direction::Forward => {
                let pairs = self.read_pairs(data_off + 4, fwd_n);
                Self::choose_from_pairs(&pairs, rng)
            }
            Direction::Reverse => {
                let rev_off = data_off + 4 + fwd_n * 8;
                let rev_n = u32::from_le_bytes(
                    self.mmap[rev_off..rev_off + 4].try_into().unwrap(),
                ) as usize;
                let pairs = self.read_pairs(rev_off + 4, rev_n);
                Self::choose_from_pairs(&pairs, rng)
            }
        }
    }

    fn iterate(&self, dir: Direction, mut prefix: (u32, u32), rng: &mut ThreadRng) -> Vec<u32> {
        let none = 0u32; // TokID 0 is always ""
        let mut result = Vec::new();
        loop {
            let choice = match self.choose_next(prefix.0, prefix.1, &dir, rng) {
                Some(c) => c,
                None => break,
            };
            if choice == none {
                break;
            }
            result.push(choice);
            prefix = match dir {
                Direction::Forward => (prefix.1, choice),
                Direction::Reverse => (choice, prefix.0),
            };
        }
        result
    }

    fn ids_to_words(&self, ids: &[u32]) -> Vec<String> {
        ids.iter().filter_map(|id| self.dict.entry(*id)).collect()
    }

    fn generate_one(&self, rng: &mut ThreadRng) -> Option<Vec<String>> {
        let none = 0u32;
        let toks = self.iterate(Direction::Forward, (none, none), rng);
        if toks.is_empty() {
            return None;
        }
        Some(self.ids_to_words(&toks))
    }

    fn generate_one_from(&self, rng: &mut ThreadRng, start: &str) -> Option<Vec<String>> {
        let mut phrase: Vec<u32> = Vec::new();
        for word in start.split_whitespace() {
            phrase.push(self.dict.get_tokid(word)?);
        }

        match phrase.len() {
            0 => return self.generate_one(rng),
            1 => {
                let entry_off = self.find_entry(phrase[0])?;
                let n = u32::from_le_bytes(
                    self.mmap[entry_off..entry_off + 4].try_into().unwrap(),
                ) as usize;
                let pairs = self.read_pairs(entry_off + 4, n);
                phrase.push(Self::choose_from_pairs(&pairs, rng)?);
            }
            _ => {}
        }

        let size = phrase.len();
        let reverse_prefix = (phrase[0], phrase[1]);
        let forward_prefix = (phrase[size - 2], phrase[size - 1]);

        let end_ids = self.iterate(Direction::Forward, forward_prefix, rng);
        let mut begin_ids = self.iterate(Direction::Reverse, reverse_prefix, rng);
        begin_ids.reverse();

        let middle = self.ids_to_words(&phrase);
        let mut result = self.ids_to_words(&begin_ids);
        result.extend(middle);
        result.extend(self.ids_to_words(&end_ids));
        Some(result)
    }

    fn choose_best(gens: Vec<Option<Vec<String>>>, target_words: i32) -> Option<Vec<String>> {
        let mut sorted: Vec<Vec<String>> = gens.into_iter().flatten().collect();
        sorted.sort_by_key(|s| ((s.len() as i32) - target_words).abs());
        sorted.into_iter().next()
    }

    pub fn generate_best(&self, target_words: i32) -> Option<String> {
        let mut rng = rng();
        let gens: Vec<_> = (1..50).map(|_| self.generate_one(&mut rng)).collect();
        Self::choose_best(gens, target_words).map(|v| v.join(" "))
    }

    pub fn generate_best_from(&self, start: &str, target_words: i32) -> Option<String> {
        let mut rng = rng();
        let gens: Vec<_> = (1..50).map(|_| self.generate_one_from(&mut rng, start)).collect();
        Self::choose_best(gens, target_words).map(|v| v.join(" "))
    }
}
