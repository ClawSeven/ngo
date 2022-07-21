use crate::prelude::*;
use bit_vec::BitVec;

#[derive(Debug, Clone, PartialEq)]
pub struct BitMask {
    bits: BitVec<u32>,
}

impl BitMask {
    /// A full set of executor vcpus.
    pub fn new_full(num_vpus: usize) -> Self {
        let bits = BitVec::from_elem(num_vpus, true);
        Self { bits }
    }

    /// A empty set of executor vcpus.
    pub fn new_empty(num_vpus: usize) -> Self {
        let bits = BitVec::from_elem(num_vpus, false);
        Self { bits }
    }

    /// Returns whether the set is full.
    pub fn is_full(&self) -> bool {
        self.bits.all()
    }

    /// Returns whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.bits.none()
    }

    /// Returns the number of vcpus in the set.
    pub fn count(&self) -> usize {
        self.bits.iter().filter(|x| *x).count()
    }

    /// Set whether the i-th vcpu is in the set.
    pub fn set(&mut self, i: usize, b: bool) {
        self.bits.set(i, b);
    }

    /// Get whether the i-th vcpu is in the set.
    pub fn get(&self, i: usize) -> bool {
        self.bits.get(i).unwrap()
    }

    /// Returns an iterator that allows accessing the underlying bits.
    pub fn iter(&self) -> impl Iterator<Item = bool> + '_ {
        self.bits.iter()
    }

    /// Returns an iterator that allows accessing all indexs of bits that are set to 1.
    pub fn iter_ones(&self) -> impl Iterator<Item = usize> + '_ {
        self.bits
            .iter()
            .enumerate()
            .filter_map(|(idx, bit)| if bit { Some(idx) } else { None })
    }

    /// Returns an iterator that allows accessing all indexs of bits that are set to 0.
    pub fn iter_zeroes(&self) -> impl Iterator<Item = usize> + '_ {
        self.bits
            .iter()
            .enumerate()
            .filter_map(|(idx, bit)| if !bit { Some(idx) } else { None })
    }

    /// Returns a best thread id according to affinity and the length of thread's run_queue.
    pub(crate) fn get_best_thread_by_length(&self, lengths: &Vec<usize>) -> usize {
        self.bits
            .iter()
            .enumerate()
            .filter_map(|(idx, bit)| if bit { Some(idx) } else { None })
            .min_by_key(|k| lengths[*k])
            .unwrap()
    }
}
