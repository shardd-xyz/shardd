use rand::seq::SliceRandom;
use std::collections::HashSet;

/// Bounded set of known peer addresses.
#[derive(Debug, Clone)]
pub struct PeerSet {
    addrs: HashSet<String>,
    max: usize,
    /// Our own address, excluded from the set.
    self_addr: String,
}

impl PeerSet {
    pub fn new(max: usize, self_addr: String) -> Self {
        Self {
            addrs: HashSet::new(),
            max,
            self_addr,
        }
    }

    /// Add a peer. Returns true if newly inserted.
    pub fn add(&mut self, addr: &str) -> bool {
        if addr == self.self_addr || self.addrs.len() >= self.max {
            return false;
        }
        self.addrs.insert(addr.to_string())
    }

    /// Merge multiple peer addresses.
    pub fn merge(&mut self, others: &[String]) {
        for addr in others {
            if self.addrs.len() >= self.max {
                break;
            }
            self.add(addr);
        }
    }

    /// Pick up to `n` random peers.
    pub fn random_sample(&self, n: usize) -> Vec<String> {
        let mut v: Vec<&String> = self.addrs.iter().collect();
        let mut rng = rand::rng();
        v.shuffle(&mut rng);
        v.into_iter().take(n).cloned().collect()
    }

    pub fn to_vec(&self) -> Vec<String> {
        let mut v: Vec<String> = self.addrs.iter().cloned().collect();
        v.sort();
        v
    }

    pub fn len(&self) -> usize {
        self.addrs.len()
    }

    pub fn contains(&self, addr: &str) -> bool {
        self.addrs.contains(addr)
    }
}
