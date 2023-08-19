use clap::Parser;
use clio::{Input, Output};
use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use nostr_db::{Db, Event, Filter, FromEventData};
use rayon::prelude::*;
use std::{
    fs::File,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

mod bench;
mod relay;

pub use bench::*;
pub use relay::*;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Db(#[from] nostr_db::Error),
    #[error(transparent)]
    Relay(#[from] nostr_relay::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Clio(#[from] clio::Error),
    #[error("event error{0}")]
    Event(String),
    #[error("{0}")]
    Message(String),
}

pub type Result<T, E = Error> = core::result::Result<T, E>;

/// import options
#[derive(Debug, Clone, Parser)]
pub struct ImportOpts {
    /// Nostr events data directory path. The "rnostr.example.toml" default setting is "data/events"
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
    /// Nostr events data directory path. The "rnostr.example.toml" default setting is "data/events"
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
    db.check_schema()?;
    let reader = BufReader::new(input);
    let lines = reader.lines();
    let mut batches = vec![];
    let mut count = 0;

    fn parse_events(batches: &Vec<String>, search: bool) -> Vec<Event> {
        batches
            .par_iter()
            .filter_map(|s| {
                let event = Event::from_data(s.as_bytes());
                match event {
                    Ok(mut event) => {
                        if search {
                            event.build_note_words();
                        }
                        Some(event)
                    }
                    Err(e) => {
                        println!("error: {} {}", s, e);
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
    let iter = db.iter::<String, _>(&reader, filter)?;
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
    let iter = db.iter::<String, _>(&reader, filter)?;
    let mut count = 0;
    for event in iter {
        count += 1;
        let mut json: String = event?;
        json.push('\n');
        output.write_all(json.as_bytes())?;
        f(count);
    }
    output.finish()?;
    Ok(count)
}
