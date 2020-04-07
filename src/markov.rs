use std::collections::HashMap;
use std::fs::File;
use std::hash::Hash;
use std::io::prelude::*;
use std::io::{self, BufReader};
use std::path::Path;
use std::sync::Arc;
use std::vec::Vec;

use rand::rngs::ThreadRng;
use rand::seq::SliceRandom;
use rand::{thread_rng, Rng};

type TokID = u32;
type Prefix1 = TokID;
type Prefix2 = (TokID, TokID);
type Token = Option<String>;
type HashTokSet = HashMap<TokID, u16>;

pub trait TokSet {
    fn new() -> Self;
    fn is_empty(&self) -> bool;
    fn add_entry(&mut self, entry: TokID);
    fn choose(&self, rng: &mut ThreadRng) -> TokID;
}

impl TokSet for HashTokSet {
    fn new() -> HashTokSet {
        HashTokSet::new()
    }

    fn is_empty(&self) -> bool {
        self.is_empty()
    }

    fn add_entry(&mut self, entry: TokID) {
        self.entry(entry).and_modify(|e| *e += 1).or_insert(1);
    }

    fn choose(&self, rng: &mut ThreadRng) -> TokID {
        let choicev: Vec<_> = self.iter().map(|(k, v)| (k, v)).collect();
        let choice = choicev.choose_weighted(rng, |e| e.1).unwrap().0;
        *choice
    }
}

#[derive(PartialEq, Debug)]
struct BufferTokSet {
    buf: Vec<u8>,
    c2: u16,
    c1: u16,
}

impl BufferTokSet {
    fn new() -> BufferTokSet {
        BufferTokSet {
            buf: Vec::new(),
            c2: 0,
            c1: 0,
        }
    }
    fn length(&self) -> usize {
        // number of tokens
        let first2 = self.c1 as usize + self.c2 as usize;
        let leftover = self.buf.len() - (self.c1 as usize) - (self.c2 as usize * 2);
        let byte3_length = leftover / 3;
        first2 + byte3_length
    }
    fn get(&self, index: usize) -> TokID {
        // [b0 b1 b2 b3 | b4 b4 b5 b5 | b6 b6 b6 b7 b7 b7 b8 b8 b8]
        //  0  1  2  3    4  5  6  7    8  9  10 11 12 13 14 15 16
        let offsets = [0, self.c1 as usize, self.c1 as usize + self.c2 as usize];
        if index < offsets[1] {
            TokID::from(self.buf[index])
        } else if index < offsets[2] {
            let offset = offsets[1];
            let shift = (index - offset) * 2;
            let i = (self.c1 as usize) + shift;
            let b1 = TokID::from(self.buf[i]);
            let b2 = TokID::from(self.buf[i + 1]) << 8;
            b1 + b2
        } else {
            let offset = offsets[2];
            let shift = (index - offset) * 3;
            let i = (self.c1 as usize) + (self.c2 as usize * 2) + shift;
            let b1 = TokID::from(self.buf[i]);
            let b2 = TokID::from(self.buf[i + 1]) << 8;
            let b3 = TokID::from(self.buf[i + 2]) << 16;
            b1 + b2 + b3
        }
    }
    fn add1(&mut self, tok: TokID) {
        // Insert at end of c1
        self.buf.insert(self.c1 as usize, tok as u8);
        self.c1 += 1;
    }
    fn add2(&mut self, tok: TokID) {
        // [b1 b2 b3 | b4 b4 b5 b5 | b6 b6 b6 b7 b7 b7]
        // Insert at end of c2
        let byte1 = tok as u8;
        let byte2 = tok >> 8 as u8;
        let insert = u32::from(self.c1) + u32::from(self.c2) * 2;
        self.buf.insert(insert as usize, byte1 as u8);
        self.buf.insert((insert + 1) as usize, byte2 as u8);
        self.c2 += 1;
    }
    fn add3(&mut self, tok: TokID) {
        let byte1 = tok as u8;
        let byte2 = tok >> 8 as u8;
        let byte3 = tok >> 16 as u8;
        self.buf.push(byte1 as u8);
        self.buf.push(byte2 as u8);
        self.buf.push(byte3 as u8);
    }
    pub fn add(&mut self, entry: TokID) {
        if entry <= 0xFF && self.c1 < 0xFFFF {
            self.add1(entry)
        } else if entry <= 0xFFFF && self.c2 < 0xFFFF {
            self.add2(entry)
        } else if entry <= 0x00FF_FFFF {
            self.add3(entry)
        } else {
            // Not supported
            panic!("4-byte entries not supported")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn small_values() {
        let mut tokset = BufferTokSet::new();
        tokset.add_entry(2);
        tokset.add_entry(7);
        tokset.add_entry(42);
        assert_eq!(2, tokset.get(0));
        assert_eq!(7, tokset.get(1));
        assert_eq!(42, tokset.get(2));
        assert_eq!(3, tokset.length());
    }
    #[test]
    fn large_values() {
        let mut tokset = BufferTokSet::new();
        tokset.add_entry(0xFFFFF);
        tokset.add_entry(1);
        tokset.add_entry(0xFF + 1);
        tokset.add_entry(42);
        println!("{:?}", tokset);
        assert_eq!(0xFFFFF, tokset.get(3));
        assert_eq!(1, tokset.get(0));
        assert_eq!(42, tokset.get(1));
        assert_eq!(0xFF + 1, tokset.get(2));
        assert_eq!(4, tokset.length());
    }
    #[test]
    fn overflow() {
        let mut tokset = BufferTokSet::new();
        for _ in 0..1000 {
            tokset.add_entry(0xFF + 1);
        }
        for _ in 0..1000 {
            tokset.add_entry(1);
        }
        assert_eq!(2000, tokset.length());
        for i in 0..1000 {
            assert_eq!(1, tokset.get(i));
        }
        for i in 1001..2000 {
            assert_eq!(0xFF + 1, tokset.get(i));
        }
    }
}

impl TokSet for BufferTokSet {
    fn new() -> BufferTokSet {
        BufferTokSet::new()
    }
    fn is_empty(&self) -> bool {
        self.length() == 0
    }

    fn add_entry(&mut self, entry: TokID) {
        self.add(entry);
    }

    fn choose(&self, rng: &mut ThreadRng) -> TokID {
        let n: usize = rng.gen_range(0, self.length());
        self.get(n)
    }
}

#[derive(PartialEq, Debug)]
pub struct Dict {
    tokenids: HashMap<Arc<Token>, TokID>,
    entries: Vec<Arc<Token>>,
}

impl Dict {
    pub fn new() -> Dict {
        Dict {
            tokenids: HashMap::new(),
            entries: Vec::new(),
        }
    }

    pub fn tokid(&mut self, token: &Token) -> TokID {
        if let Some(found) = self.tokenids.get(token) {
            return *found;
        }
        let owntoken = Arc::new(token.to_owned());
        let tokid = self.entries.len() as TokID;
        self.tokenids.insert(owntoken.clone(), tokid);
        self.entries.push(owntoken);
        tokid
    }

    pub fn entry(&self, token_id: TokID) -> Option<&Token> {
        self.entries.get(token_id as usize).map(|l| &**l)
    }
}

pub trait Prefix: Eq + Hash + Clone {
    fn size() -> usize;
    fn entrypoint(dict: &mut Dict) -> Self;
}

impl Prefix for Prefix1 {
    fn size() -> usize {
        1
    }

    fn entrypoint(dict: &mut Dict) -> Prefix1 {
        dict.tokid(&None)
    }
}

impl Prefix for Prefix2 {
    fn size() -> usize {
        2
    }

    fn entrypoint(dict: &mut Dict) -> Prefix2 {
        let none = dict.tokid(&None);
        (none, none)
    }
}

pub enum Direction {
    Forward,
    Reverse,
}

#[derive(Debug)]
struct NextTokens {
    forward: BufferTokSet,
    reverse: BufferTokSet,
}

impl NextTokens {
    pub fn new() -> NextTokens {
        NextTokens {
            forward: BufferTokSet::new(),
            reverse: BufferTokSet::new(),
        }
    }
}

#[derive(Debug)]
struct TokenPaths {
    maps: HashMap<TokID, HashMap<TokID, NextTokens>>,
}

impl TokenPaths {
    fn new() -> TokenPaths {
        TokenPaths {
            maps: HashMap::new(),
        }
    }

    fn append(&mut self, prefix: Prefix2, forward_value: TokID, reverse_value: TokID) {
        let nested = self.maps.entry(prefix.0).or_insert_with(HashMap::new);
        let toksets = nested.entry(prefix.1).or_insert_with(NextTokens::new);
        toksets.forward.add_entry(forward_value);
        toksets.reverse.add_entry(reverse_value);
    }

    fn get(&self, prefix: Prefix2) -> Option<&NextTokens> {
        self.maps
            .get(&prefix.0)
            .and_then(|nested| nested.get(&prefix.1))
    }

    fn iterator(&self, direction: Direction, start: Prefix2) -> TokenIter {
        TokenIter {
            paths: &self,
            direction,
            prefix: start,
            rng: thread_rng(),
        }
    }
}

struct TokenIter<'a> {
    paths: &'a TokenPaths,
    direction: Direction,
    rng: ThreadRng,
    prefix: (TokID, TokID),
}

impl<'a> Iterator for TokenIter<'a> {
    type Item = TokID;

    fn next(&mut self) -> Option<TokID> {
        use Direction::{Forward, Reverse};
        let toksets = self.paths.get(self.prefix)?;

        let m = match self.direction {
            Forward => &toksets.forward,
            Reverse => &toksets.reverse,
        };

        let choice = m.choose(&mut self.rng);

        self.prefix = match self.direction {
            Forward => (self.prefix.1, choice),
            Reverse => (choice, self.prefix.0),
        };

        Some(choice)
    }
}

type Entries = HashMap<TokID, BufferTokSet>;

#[derive(Debug)]
pub struct Chain {
    dict: Dict,
    paths: TokenPaths,
    entries: Entries,
}

impl Chain {
    pub fn new() -> Chain {
        Chain {
            paths: TokenPaths::new(),
            dict: Dict::new(),
            entries: HashMap::new(),
        }
    }

    pub fn printsizes(&self) {
        println!(
            "Chain[dict: {}, paths: {}, entries: {}]",
            self.dict.entries.len(),
            self.paths.maps.len(),
            self.entries.len()
        );
    }

    pub fn feed(&mut self, tokens: Vec<String>) -> &mut Chain {
        if tokens.is_empty() {
            return self;
        }
        let none = self.dict.tokid(&None);
        let mut toks = vec![none, none, none];
        toks.extend(tokens.into_iter().map(|t| self.dict.tokid(&Some(t))));
        toks.push(none);
        toks.push(none);
        for p in toks.windows(4) {
            if let [a, b, c, d] = *p {
                let prefix = (b, c);
                self.paths.append(prefix, d, a);

                let eprefix: Prefix1 = b;
                let etokset = self.entries.entry(eprefix).or_insert_with(TokSet::new);
                etokset.add_entry(c);
            }
        }
        self
    }

    pub fn feed_str(&mut self, string: &str) -> &mut Chain {
        let words = string
            .split_whitespace()
            .filter(|word| !word.is_empty())
            .map(|s| s.to_owned())
            .collect();
        self.feed(words)
    }

    pub fn feed_file<Y: AsRef<Path>>(&mut self, path: Y) -> io::Result<&mut Chain> {
        let reader = BufReader::new(File::open(path)?);
        for line in reader.lines() {
            let line = line?;
            self.feed_str(&line);
        }
        Ok(self)
    }

    fn vec_to_string(vec: Vec<String>) -> String {
        let mut ret = String::new();
        for s in &vec {
            ret.push_str(&s);
            ret.push_str(" ");
        }
        let len = ret.len();
        if len > 0 {
            ret.truncate(len - 1);
        }
        ret
    }

    pub fn generate_from_prefix(&mut self, dir: Direction, prefix: Prefix2) -> Vec<String> {
        let mut ret = vec![];

        if self.paths.get(prefix).is_none() {
            return ret;
        }

        let none = self.dict.tokid(&None);

        if let Some(Some(word)) = self.dict.entry(prefix.0) {
            ret.push(word.clone());
        }

        if let Some(Some(word)) = self.dict.entry(prefix.1) {
            ret.push(word.clone());
        }

        let iter = self
            .paths
            .iterator(dir, prefix)
            .take_while(|i| *i != none)
            .filter_map(|x| self.dict.entry(x))
            .filter_map(|x| x.clone());

        ret.extend(iter);

        ret
    }

    pub fn generate_one(&mut self) -> Option<String> {
        let none = self.dict.tokid(&None);
        let result = self.generate_from_prefix(Direction::Forward, (none, none));
        Some(Chain::vec_to_string(result))
    }

    pub fn generate_one_from(&mut self, rng: &mut ThreadRng, start: &str) -> Option<String> {
        let s = self.dict.tokid(&Some(start.to_string()));
        if let Some(possibles) = self.entries.get(&s) {
            let next_start = possibles.choose(rng);
            let prefix = (s, next_start);

            let forward = self.generate_from_prefix(Direction::Forward, prefix);
            let mut reverse = self.generate_from_prefix(Direction::Reverse, prefix);
            reverse.reverse();
            reverse.pop();
            reverse.pop();
            reverse.extend(forward);
            Some(Chain::vec_to_string(reverse))
        } else {
            None
        }
    }

    fn choose_best(gens: Vec<Option<String>>, target_chars: i32) -> Option<String> {
        let mut sorted = gens.into_iter().filter_map(|o| o).collect::<Vec<_>>();
        sorted.sort_by_key(|s| ((s.len() as i32) - target_chars).abs());
        if sorted.is_empty() {
            None
        } else {
            Some(sorted[0].clone())
        }
    }

    pub fn generate_best_from(&mut self, start: String, target_chars: i32) -> Option<String> {
        let mut rng = thread_rng();
        let gens: Vec<_> = (1..50)
            .map(|_| self.generate_one_from(&mut rng, &start[..]))
            .collect();
        Self::choose_best(gens, target_chars)
    }

    pub fn generate_best(&mut self, target_chars: i32) -> Option<String> {
        let gens: Vec<_> = (1..50).map(|_| self.generate_one()).collect();
        Self::choose_best(gens, target_chars)
    }
}
