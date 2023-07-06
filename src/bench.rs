use crate::Result;
use clap::Parser;
use nostr_db::{Db, Filter, Stats};
use rayon::prelude::*;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

/// bench options
#[derive(Debug, Clone, Parser)]
pub struct BenchOpts {
    /// Nostr events data directory path. The "rnostr.example.toml" default setting is "data/events"
    #[arg(value_name = "PATH")]
    pub path: PathBuf,

    /// [NIP-01](https://nips.be/1) Filter
    #[arg(short = 'f', long, value_name = "FILTER", default_value = "{}")]
    pub filter: Filter,

    /// only bench the count method
    #[arg(long, value_name = "BOOL")]
    pub count: bool,
}

pub fn bench_opts(mut opts: BenchOpts) -> anyhow::Result<u64> {
    opts.filter.build_words();
    let count = bench(&opts.path, &opts.filter, opts.count)?;
    Ok(count)
}

pub fn bench(path: &PathBuf, filter: &Filter, count: bool) -> Result<u64> {
    fn once(db: &Db, filter: &Filter, count: bool) -> Result<(u64, Stats)> {
        let reader = db.reader()?;
        let mut iter = db.iter::<String, _>(&reader, filter)?;
        if count {
            Ok(iter.size()?)
        } else {
            let mut c = 0;
            for event in iter.by_ref() {
                let _json: String = event?;
                c += 1;
            }
            Ok((c, iter.stats()))
        }
    }

    let db = Db::open(path)?;
    let now = Instant::now();
    let res = once(&db, filter, count)?;
    let elapsed = now.elapsed();

    println!("{:?}", filter);
    println!("Size: {:?}", res.0);
    println!("{:?}", res.1);
    println!("Time: {:?}, {}", elapsed, fmt_per_sec(1, &elapsed));
    let mut times = (Duration::from_secs(2).as_nanos() / elapsed.as_nanos()) as u64;
    if times == 0 {
        times = 10;
    }

    println!("Bench prepare");
    let now = Instant::now();
    for _i in 0..times {
        let _r = once(&db, filter, count)?;
    }
    let elapsed = now.elapsed();
    println!(
        "Time: {:?}, {}",
        elapsed / times as u32,
        fmt_per_sec(times, &elapsed)
    );

    println!("Bench single threaded");
    // start bench
    let mut times = Duration::from_secs(5).as_nanos() as u64 * times / elapsed.as_nanos() as u64;
    if times == 0 {
        times = 10;
    }

    let now = Instant::now();
    for _i in 0..times {
        let _r = once(&db, filter, count)?;
    }
    let elapsed = now.elapsed();
    println!(
        "Time: {:?}, {}",
        elapsed / times as u32,
        fmt_per_sec(times, &elapsed)
    );

    println!("Bench multi-threaded");
    let now = Instant::now();
    (0..times).into_par_iter().for_each(|_| {
        let _r = once(&db, filter, count);
        if let Err(e) = _r {
            println!("{:?}", e);
        }
    });
    let elapsed = now.elapsed();
    println!(
        "Time: {:?}, {}",
        elapsed / times as u32,
        fmt_per_sec(times, &elapsed)
    );
    Ok(res.0)
}

pub fn fmt_num(count: f64) -> String {
    if count < 1_000.0 {
        format!("{:.1}", count)
    } else if count < 1_000_000.0 {
        format!("{:.1}K", count / 1_000.0)
    } else if count < 1_000_000_000.0 {
        format!("{:.1}M", count / 1_000_000.0)
    } else {
        format!("{:.1}G", count / 1_000_000_000.0)
    }
}

pub fn fmt_per_sec(count: u64, dur: &Duration) -> String {
    let count = (count as f64) / (dur.as_nanos() as f64) * 1_000_000_000.0;
    format!("{}/s", fmt_num(count))
}
