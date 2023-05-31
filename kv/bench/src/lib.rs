use rand::{distributions::Alphanumeric, Rng};
use rayon::prelude::*;
use std::time::Duration;

pub fn chunk_vec<T: Clone>(vec: &Vec<T>, size: usize) -> Vec<Vec<T>> {
    vec.chunks(size).map(|chunk| chunk.to_vec()).collect()
}

pub fn gen_pairs(klen: usize, vlen: usize, len: usize) -> Vec<(Vec<u8>, Vec<u8>)> {
    (0..len)
        .into_par_iter()
        .map(|_| (gen_byte(klen), gen_str(vlen)))
        .collect()
}

pub fn gen_num_pair() -> (Vec<u8>, Vec<u8>) {
    let mut rng = rand::thread_rng();
    (
        rng.gen::<u64>().to_be_bytes().to_vec(),
        rng.gen::<u64>().to_be_bytes().to_vec(),
    )
}

pub fn gen_byte(len: usize) -> Vec<u8> {
    let mut rng = rand::thread_rng();
    (0..len).map(|_| rng.gen::<u8>()).collect()
}

pub fn gen_str(len: usize) -> Vec<u8> {
    let mut rng = rand::thread_rng();
    (0..len).map(|_| rng.sample(Alphanumeric)).collect()
}

pub fn fmt_num(count: f64) -> String {
    let ret = if count < 1_000.0 {
        format!("{:.1}", count)
    } else if count < 1_000_000.0 {
        format!("{:.1}K", count / 1_000.0)
    } else if count < 1_000_000_000.0 {
        format!("{:.1}M", count / 1_000_000.0)
    } else {
        format!("{:.1}G", count / 1_000_000_000.0)
    };
    ret
}

pub fn fmt_per_sec(count: usize, dur: &Duration) -> String {
    let count = (count as f64) / (dur.as_nanos() as f64) * 1_000_000_000.0;
    format!("{}/s", fmt_num(count))
}

#[cfg(test)]
mod tests {
    #[test]
    fn generate() {
        use super::*;
        let pairs = gen_pairs(5, 10, 15);
        assert_eq!(pairs.len(), 15);
        assert_eq!(pairs[0].0.len(), 5);
        assert_eq!(pairs[0].1.len(), 10);
    }

    #[test]
    fn fmt() {
        use super::fmt_per_sec;
        use std::time::Duration;
        assert_eq!(fmt_per_sec(10, &Duration::from_secs(1)), "10.0/s");
        assert_eq!(fmt_per_sec(1100, &Duration::from_secs(1)), "1.1K/s");
        assert_eq!(fmt_per_sec(1100_000, &Duration::from_secs(1)), "1.1M/s");
        assert_eq!(fmt_per_sec(1100_000_000, &Duration::from_secs(1)), "1.1G/s");
    }
}
