use hashbrown::HashMap;
use std::fs::File;
use std::hash::Hash;
use std::io::prelude::*;
use std::io::{self, BufReader};
use std::path::Path;
use std::rc::Rc;
use std::vec::Vec;

use rand::seq::SliceRandom;
use rand::{Rng,thread_rng};
use rand::rngs::ThreadRng;

type TokID = u32;
type Prefix1 = TokID;
type Prefix2 = (TokID, TokID);
type Token = Option<String>;
type HashTokSet = HashMap<TokID, u16>;

pub trait TokSet {
    fn new() -> Self;
    fn add_entry(&mut self, entry: TokID);
    fn choose(&self, rng: &mut ThreadRng) -> TokID;
}

impl TokSet for HashTokSet {
    fn new() -> HashTokSet {
        HashTokSet::new()
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
    c1: u16
}

impl BufferTokSet {
    fn new() -> BufferTokSet {
        BufferTokSet {
            buf: Vec::new(),
            c2: 0,
            c1: 0
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
        let offsets = [0,
                       self.c1 as usize,
                       self.c1 as usize + self.c2 as usize];
        if index < offsets[1] {
            TokID::from(self.buf[index])
        } else if index < offsets[2] {
            let offset = offsets[1];
            let shift = (index - offset) * 2;
            let i = (self.c1 as usize) + shift;
            let b1 = TokID::from(self.buf[i]);
            let b2 = TokID::from(self.buf[i+1]) << 8;
            b1 + b2
        } else {
            let offset = offsets[2];
            let shift = (index - offset) * 3;
            let i = (self.c1 as usize) + (self.c2 as usize * 2) + shift;
            let b1 = TokID::from(self.buf[i]);
            let b2 = TokID::from(self.buf[i+1]) << 8;
            let b3 = TokID::from(self.buf[i+2]) << 16;
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
        let byte2 = tok>>8 as u8;
        let insert = u32::from(self.c1) + u32::from(self.c2) * 2;
        self.buf.insert(insert as usize, byte1 as u8);
        self.buf.insert((insert + 1) as usize, byte2 as u8);
        self.c2 += 1;
    }
    fn add3(&mut self, tok: TokID) {
        let byte1 = tok as u8;
        let byte2 = tok>>8 as u8;
        let byte3 = tok>>16 as u8;
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
    tokenids: HashMap<Rc<Token>, TokID>,
    entries: Vec<Rc<Token>>,
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
        let owntoken = Rc::new(token.to_owned());
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
    Reverse
}

#[derive(Debug)]
pub struct Chain
{
    rng: ThreadRng,
    dict: Dict,
    forward: HashMap<Prefix2, BufferTokSet>,
    reverse: HashMap<Prefix2, BufferTokSet>,
    entries: HashMap<Prefix1, BufferTokSet>,
}

impl Chain
{
    pub fn new() -> Chain {
        let mut dict = Dict::new();
        let entry = Prefix2::entrypoint(&mut dict);
        Chain {
            rng: thread_rng(),
            dict,
            forward: {
                let mut map = HashMap::new();
                map.insert(entry, TokSet::new());
                map
            },
            reverse: HashMap::new(),
            entries: HashMap::new()
        }
    }

    pub fn printsizes(&self) {
        println!("Chain[dict: {}, forward: {}, reverse: {}, entries: {}]",
                 self.dict.entries.len(),
                 self.forward.len(),
                 self.reverse.len(),
                 self.entries.len());
    }

    pub fn feed(&mut self, tokens: Vec<String>) -> &mut Chain {
        if tokens.is_empty() {
            return self;
        }
        let none = self.dict.tokid(&None);
        let mut toks = vec![none, none];
        toks.extend(tokens.into_iter().map(|t| self.dict.tokid(&Some(t))));
        toks.push(none);
        for p in toks.windows(3) {
            if let &[a, b, c] = p {
                let fprefix = (a,b);
                let ftokset = self.forward.entry(fprefix).or_insert_with(TokSet::new);
                ftokset.add_entry(c);

                let rprefix = (c,b);
                let rtokset = self.reverse.entry(rprefix).or_insert_with(TokSet::new);
                rtokset.add_entry(a);

                let eprefix: Prefix1 = a;
                let etokset = self.entries.entry(eprefix).or_insert_with(TokSet::new);
                etokset.add_entry(b);
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
        let m = match dir {
            Direction::Forward => &self.forward,
            Direction::Reverse => &self.reverse
        };
        let mut ret = vec![];

        if !m.contains_key(&prefix) {
            return ret;
        }

        if let Some(Some(word)) = self.dict.entry(prefix.0) {
            ret.push(word.clone());
        }

        if let Some(Some(word)) = self.dict.entry(prefix.1) {
            ret.push(word.clone());
        }

        let mut curs = prefix;

        while let Some(tokset) = m.get(&curs) {
            let choice = tokset.choose(&mut self.rng);
            curs = (curs.1, choice);
            if let Some(Some(word)) = self.dict.entry(choice) {
                ret.push(word.clone());
            }
        }

        ret
    }

    pub fn generate_one(&mut self) -> Option<String> {
        let none = self.dict.tokid(&None);
        let result = self.generate_from_prefix(Direction::Forward, (none, none));
        Some(Chain::vec_to_string(result))
    }

    pub fn generate_one_from(&mut self, start: &str) -> Option<String> {
        let s = self.dict.tokid(&Some(String::from(start).clone()));
        if let Some(possibles) = self.entries.get(&s) {
            let next_start = possibles.choose(&mut self.rng);
            let fprefix = (s, next_start);
            let rprefix = (fprefix.1, fprefix.0);

            let forward = self.generate_from_prefix(Direction::Forward, fprefix);
            let mut reverse = self.generate_from_prefix(Direction::Reverse, rprefix);
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
        let mut sorted = gens.into_iter()
            .filter_map(|o| o)
            .collect::<Vec<_>>();
        sorted.sort_by_key(|s| ((s.len() as i32) - target_chars).abs());
        if sorted.is_empty() {
            None
        } else {
            Some(sorted[0].clone())
        }
    }

    pub fn generate_best_from(&mut self, start: String, target_chars: i32) -> Option<String> {
        let gen1 = self.generate_one_from(&start[..]);
        if let Some(_) = gen1 {
            let mut gens = vec![gen1];
            gens.extend((1..50).map(|_| self.generate_one_from(&start[..])));
            Self::choose_best(gens, target_chars)
        } else {
            None
        }
    }

    pub fn generate_best(&mut self, target_chars: i32) -> Option<String> {
        let gen1 = self.generate_one();
        if let Some(_) = gen1 {
            let mut gens = vec![gen1];
            gens.extend((1..50).map(|_| self.generate_one()));
            Self::choose_best(gens, target_chars)
        } else {
            None
        }
    }
}
