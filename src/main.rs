use std::{
    fmt, fs,
    io::{prelude::*, Cursor, SeekFrom},
};

use anyhow::{bail, Context, Result};
use binrw::prelude::*;
use camino::{Utf8Path, Utf8PathBuf};
use clap::{ArgAction, Parser};
use log::*;
use memmap2::Mmap;

#[derive(Debug, Parser)]
struct Args {
    /// Where's all the data?
    wad: Utf8PathBuf,

    /// Verbosity (-v, -vv, -vvv, etc.)
    #[clap(short, long, action(ArgAction::Count))]
    verbose: u8,

    #[clap(short, long, value_enum, default_value = "auto")]
    color: Color,

    /// Prepend ISO-8601 timestamps to all trace messages (from --verbose).
    /// Useful for benchmarking.
    #[clap(short, long, verbatim_doc_comment)]
    timestamps: bool,
}

#[derive(Debug, Copy, Clone, clap::ValueEnum)]
enum Color {
    Auto,
    Always,
    Never,
}

fn doomstr(d: &[u8]) -> &str {
    let nulterm = d.iter().take_while(|b| **b != 0).count();
    std::str::from_utf8(&d[..nulterm]).expect("Non-ASCII in WAD")
}

fn map_wad(wad: &Utf8Path) -> Result<Mmap> {
    let fd = fs::File::open(wad).context("Couldn't open WAD")?;
    let mmap = unsafe { Mmap::map(&fd).context("Couldn't map WAD")? };
    Ok(mmap)
}

#[binrw]
struct Wadinfo {
    magic: [u8; 4],
    numlumps: u32,
    infotableofs: u32,
}

fn go(args: Args) -> Result<()> {
    let wad_map = map_wad(&args.wad)?;
    let mut curse = Cursor::new(wad_map);
    let curse = &mut curse;

    let info: Wadinfo = curse.read_le()?;
    let magic = doomstr(&info.magic);
    if magic != "IWAD" && magic != "PWAD" {
        bail!("Bad magic: {magic}");
    }
    let dir = read_directory(curse, &info)?;
    info!("{} lumps:", info.numlumps);
    for lump in &dir {
        info!("  {}", doomstr(&lump.name));
    }

    Ok(())
}

#[binrw]
struct Filelump {
    filepos: u32,
    size: u32,
    name: [u8; 8],
}

impl fmt::Debug for Filelump {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Filelump")
            .field("filepos", &self.filepos)
            .field("size", &self.size)
            .field("name", &doomstr(&self.name))
            .finish()
    }
}

fn read_directory(c: &mut std::io::Cursor<Mmap>, wi: &Wadinfo) -> Result<Vec<Filelump>> {
    c.seek(SeekFrom::Start(wi.infotableofs as u64))?;
    let mut lumps = Vec::with_capacity(wi.numlumps as usize);
    for _i in 0..wi.numlumps {
        lumps.push(c.read_le()?);
    }
    Ok(lumps)
}

fn main() {
    let args = Args::parse();
    init_logger(&args);
    if let Err(e) = go(args) {
        error!("{}", e);
        std::process::exit(1);
    }
}

fn init_logger(args: &Args) {
    use simplelog::{ColorChoice, ConfigBuilder, LevelPadding, TermLogger, TerminalMode};

    let mut builder = ConfigBuilder::new();
    builder.set_target_level(LevelFilter::Off);
    builder.set_thread_level(LevelFilter::Off);
    if args.timestamps {
        builder.set_time_format_rfc3339();
        builder.set_time_level(LevelFilter::Error);
    } else {
        builder.set_time_level(LevelFilter::Off);
    }

    let level = match args.verbose {
        0 => LevelFilter::Warn,
        1 => LevelFilter::Info,
        2 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    };

    if level == LevelFilter::Trace {
        builder.set_location_level(LevelFilter::Error);
    }
    builder.set_level_padding(LevelPadding::Left);

    let config = builder.build();

    let color = match args.color {
        Color::Always => ColorChoice::AlwaysAnsi,
        Color::Auto => {
            if atty::is(atty::Stream::Stderr) {
                ColorChoice::Auto
            } else {
                ColorChoice::Never
            }
        }
        Color::Never => ColorChoice::Never,
    };

    TermLogger::init(level, config.clone(), TerminalMode::Stderr, color)
        .context("Couldn't init logger")
        .unwrap()
}
