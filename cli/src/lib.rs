use clap::Parser;
use clio::{Input, Output};
use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use nostr_db::{Db, Event, Filter, FromEventJson, Stats};
use rayon::prelude::*;
use std::{
    fs::File,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    println,
    time::{Duration, Instant},
};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Db(#[from] nostr_db::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Clio(#[from] clio::Error),
    #[error("event error{0}")]
    Event(String),
    #[error("{0}")]
    Message(String),
}

type Result<T, E = Error> = core::result::Result<T, E>;

/// import options
#[derive(Debug, Clone, Parser)]
pub struct ImportOpts {
    /// Nostr db path
    #[arg(value_name = "PATH")]
    pub path: PathBuf,

    /// Support search
    #[arg(long, value_name = "BOOL")]
    pub search: bool,

    /// input jsonl data file, use '-' for stdin
    #[clap(value_parser, default_value = "-")]
    pub input: Input,
}

/// export options
#[derive(Debug, Clone, Parser)]
pub struct ExportOpts {
    /// Nostr db path
    #[arg(value_name = "PATH")]
    pub path: PathBuf,

    /// [NIP-01](https://nips.be/1) Filter
    #[arg(short = 'f', long, value_name = "FILTER", default_value = "{}")]
    pub filter: Filter,

    /// overwrite order in the filter, By default, if the filter provides a limit, it will order by time descending, otherwise ascending
    #[arg(long, value_name = "BOOL")]
    pub desc: Option<bool>,

    /// output jsonl data file, use '-' for stdout
    #[clap(value_parser, default_value = "-")]
    pub output: Output,
}

/// bench options
#[derive(Debug, Clone, Parser)]
pub struct BenchOpts {
    /// Nostr db path
    #[arg(value_name = "PATH")]
    pub path: PathBuf,

    /// [NIP-01](https://nips.be/1) Filter
    #[arg(short = 'f', long, value_name = "FILTER", default_value = "{}")]
    pub filter: Filter,

    /// only bench the count method
    #[arg(long, value_name = "BOOL")]
    pub count: bool,
}

/// import
pub fn import_opts(opts: ImportOpts) -> anyhow::Result<usize> {
    fn run_import_opts<F: Fn(usize)>(opts: ImportOpts, f: F) -> anyhow::Result<usize> {
        let count = import(&opts.path, opts.input, 10000, opts.search, f)?;
        Ok(count)
    }

    if matches!(opts.input, Input::File(_, _)) {
        let path = opts.input.path();
        let total_size = count_lines(path)? as u64;
        let pb = create_pb(total_size);
        let total = run_import_opts(opts, |c| {
            if c % 1000 == 0 {
                pb.set_position(c as u64);
            }
        })?;
        pb.finish_with_message("finished");
        Ok(total)
    } else {
        run_import_opts(opts, |_| {})
    }
}

fn count_lines<P: AsRef<Path>>(path: P) -> std::io::Result<usize> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let lines = reader.lines();
    Ok(lines.count())
}

pub fn import<F: Fn(usize)>(
    path: &PathBuf,
    input: Input,
    batch: usize,
    search: bool,
    f: F,
) -> Result<usize> {
    let db = Db::open(path)?;
    let reader = BufReader::new(input);
    let lines = reader.lines();
    let mut batches = vec![];
    let mut count = 0;

    fn parse_events(batches: &Vec<String>, search: bool) -> Vec<Event> {
        batches
            .par_iter()
            .filter_map(|s| {
                let event = Event::from_json(s.as_bytes());
                match event {
                    Ok(mut event) => {
                        if search {
                            event.build_words();
                        }
                        Some(event)
                    }
                    Err(e) => {
                        println!("error: {} {}", s, e.to_string());
                        None
                    }
                }
            })
            .collect()
    }
    let parse_batch = 30;
    let mut writer = db.writer()?;
    for item in lines.enumerate() {
        let line = item.1?;
        let index = item.0;
        if index > 0 && index % parse_batch == 0 {
            // batch write
            // count += db.batch_put()?;
            let events = parse_events(&batches, search);
            for event in events {
                db.put(&mut writer, event)?;
                count += 1;
            }
            batches.clear();
        }
        batches.push(line);
        if index > 0 && index % batch == 0 {
            db.commit(writer)?;
            writer = db.writer()?;
        }
        f(index);
    }

    db.commit(writer)?;

    db.batch_put(parse_events(&batches, search))?;
    count += batches.len();
    db.flush()?;
    Ok(count)
}

fn create_pb(total: u64) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({eta})",
        )
        .unwrap()
        // .with_key("eta",  |state, w| write!(w, "{:.1}s", state.eta().as_secs_f64()).unwrap())
        .with_key(
            "eta",
            |state: &ProgressState, w: &mut dyn std::fmt::Write| {
                write!(w, "{:.1}s", state.eta().as_secs_f64()).unwrap()
            },
        )
        .progress_chars("#>-"),
    );
    pb
}

pub fn export_opts(opts: ExportOpts) -> anyhow::Result<usize> {
    fn run_export_opts<F: Fn(usize)>(mut opts: ExportOpts, f: F) -> anyhow::Result<usize> {
        opts.filter.build_words();
        if let Some(desc) = opts.desc {
            opts.filter.desc = desc;
        }
        let count = export(&opts.path, opts.output, &opts.filter, f)?;
        Ok(count)
    }

    if matches!(opts.output, Output::File(_, _)) {
        let total_size = count(&opts.path, &opts.filter)?;
        let pb = create_pb(total_size);
        let total = run_export_opts(opts, |c| {
            if c % 1000 == 0 {
                pb.set_position(c as u64);
            }
        })?;
        pb.finish_with_message("finished");
        Ok(total)
    } else {
        run_export_opts(opts, |_| {})
    }
}

pub fn count(path: &PathBuf, filter: &Filter) -> Result<u64> {
    let db = Db::open(path)?;
    let reader = db.reader()?;
    let iter = db.iter::<String, _>(&reader, &filter)?;
    Ok(iter.size()?.0)
}

pub fn export<F: Fn(usize)>(
    path: &PathBuf,
    mut output: Output,
    filter: &Filter,
    f: F,
) -> Result<usize> {
    let db = Db::open(path)?;
    let reader = db.reader()?;
    let mut iter = db.iter::<String, _>(&reader, &filter)?;
    let mut count = 0;
    while let Some(event) = iter.next() {
        count += 1;
        let mut json: String = event?;
        json.push_str("\n");
        output.write(json.as_bytes())?;
        f(count);
    }
    output.finish()?;
    Ok(count)
}

pub fn bench_opts(mut opts: BenchOpts) -> anyhow::Result<u64> {
    opts.filter.build_words();
    let count = bench(&opts.path, &opts.filter, opts.count)?;
    Ok(count)
}

pub fn bench(path: &PathBuf, filter: &Filter, count: bool) -> Result<u64> {
    fn once(db: &Db, filter: &Filter, count: bool) -> Result<(u64, Stats)> {
        let reader = db.reader()?;
        let mut iter = db.iter::<String, _>(&reader, &filter)?;
        if count {
            Ok(iter.size()?)
        } else {
            let mut c = 0;
            while let Some(event) = iter.next() {
                let _json: String = event?;
                c += 1;
            }
            Ok((c, iter.stats()))
        }
    }

    let db = Db::open(path)?;
    let now = Instant::now();
    let res = once(&db, &filter, count)?;
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
        let _r = once(&db, &filter, count)?;
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
        let _r = once(&db, &filter, count)?;
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
        let _r = once(&db, &filter, count);
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

pub fn fmt_per_sec(count: u64, dur: &Duration) -> String {
    let count = (count as f64) / (dur.as_nanos() as f64) * 1_000_000_000.0;
    format!("{}/s", fmt_num(count))
}
