use std::{
    fs,
    io::{prelude::*, BufWriter, Cursor, SeekFrom},
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

fn map_wad(wad: &Utf8Path) -> Result<Mmap> {
    let fd = fs::File::open(wad).context("Couldn't open WAD")?;
    let mmap = unsafe { Mmap::map(&fd).context("Couldn't map WAD")? };
    Ok(mmap)
}

#[binread]
struct Wadinfo {
    magic: [u8; 4],
    numlumps: u32,
    infotableofs: u32,
}

fn doomstr(d: &[u8]) -> &str {
    let nulterm = d.iter().take_while(|b| **b != 0).count();
    std::str::from_utf8(&d[..nulterm]).expect("Non-ASCII in WAD")
}

// Determined by recording all used colors,
// but we don't want to two-pass the whole WAD just
// to rediscover this at runtime
const TRANSPARENT: u8 = 251;

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
    debug!("{} lumps", info.numlumps);
    if max_level() == LevelFilter::Trace {
        for lump in &dir {
            trace!("  {}", lump.name());
        }
    }

    let palette = dir
        .iter()
        .find(|l| l.name() == "PLAYPAL")
        .expect("No palette");
    let palette = read_palette(curse, palette)?;

    let used_colors: &mut [bool] = &mut [false; 256];

    let sprites = dir
        .iter()
        .skip_while(|l| l.name() != "S_START")
        .skip(1)
        .take_while(|l| l.name() != "S_END");

    info!("Sprites:");
    for s in sprites {
        info!("  {}", s.name());
        save_sprite(curse, s, &palette, used_colors)?;
    }

    let faces = dir.iter().filter(|l| l.name().starts_with("STF"));
    info!("Faces:");
    for f in faces {
        info!("  {}", f.name());
        save_sprite(curse, f, &palette, used_colors)?;
    }

    // We can use these for transparency
    let unused_color_indexes = used_colors
        .iter()
        .enumerate()
        .filter_map(|(i, c)| (!c).then_some(i as u8))
        .collect::<Vec<_>>();
    debug!("Unused colors: {:?}", unused_color_indexes);
    assert!(unused_color_indexes.iter().any(|b| *b == TRANSPARENT));

    Ok(())
}

#[binread]
struct Filelump {
    filepos: u32,
    _size: u32,
    namebuf: [u8; 8],
}

impl Filelump {
    fn name(&self) -> &str {
        doomstr(&self.namebuf)
    }
}

fn read_directory(c: &mut Cursor<Mmap>, wi: &Wadinfo) -> Result<Vec<Filelump>> {
    c.seek(SeekFrom::Start(wi.infotableofs as u64))?;
    let mut lumps = Vec::with_capacity(wi.numlumps as usize);
    for _i in 0..wi.numlumps {
        let lump = c.read_le()?;
        lumps.push(lump);
    }
    Ok(lumps)
}

fn read_palette(c: &mut Cursor<Mmap>, lump: &Filelump) -> Result<Vec<u8>> {
    c.seek(SeekFrom::Start(lump.filepos as u64))?;
    let mut pal = vec![0; 256 * 3];
    c.read_exact(&mut pal)?;
    Ok(pal)
}

#[derive(BinRead, Debug)]
struct PatchHeader {
    width: u16,
    height: u16,
    _leftoffset: i16,
    _topoffset: i16,
    #[br(count = width)]
    columnofs: Vec<u32>,
}

#[binrw]
struct PostHeader {
    topdelta: u8,
    #[br(pad_after = 1)]
    length: u8,
}

fn save_sprite(
    c: &mut Cursor<Mmap>,
    sprite: &Filelump,
    palette: &[u8],
    used_colors: &mut [bool],
) -> Result<()> {
    let base = sprite.filepos as u64;
    c.seek(SeekFrom::Start(base))?;

    let header: PatchHeader = c.read_le()?;
    trace!("    {header:?}");

    let mut pixels = vec![TRANSPARENT; header.width as usize * header.height as usize];

    for (x, col) in header.columnofs.iter().enumerate() {
        // trace!("      column {x}:");
        c.seek(SeekFrom::Start(base + *col as u64))?;
        loop {
            // trace!("At {}", c.position());
            let post: PostHeader = c.read_le()?;
            if post.topdelta == 255 {
                // trace!("        EOC");
                break;
            }
            /*
            trace!(
                "        [{}..{}]",
                post.topdelta,
                post.topdelta as u32 + post.length as u32
            );
            */
            for dy in 0..post.length {
                let px = read_u8(c)?;
                // trace!("          [{}] = {px}", post.topdelta + dy);
                used_colors[px as usize] = true;
                pixels[x + (post.topdelta + dy) as usize * header.width as usize] = px;
            }
            let _pad = read_u8(c)?;
        }
    }

    let outname = sprite.name().to_owned() + ".png";
    let mut encoder = png::Encoder::new(
        BufWriter::new(fs::File::create(outname)?),
        header.width as u32,
        header.height as u32,
    );
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_palette(palette);

    // TRNS: Everything is opaque but the TRANSPARENT color
    let mut trns = vec![255; 255];
    trns[TRANSPARENT as usize] = 0;
    encoder.set_trns(&trns);

    encoder.write_header()?.write_image_data(&pixels)?;

    Ok(())
}

fn read_u8(c: &mut Cursor<Mmap>) -> Result<u8> {
    let mut buf = [0; 1];
    c.read_exact(&mut buf)?;
    Ok(buf[0])
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

    TermLogger::init(level, config, TerminalMode::Stderr, color)
        .context("Couldn't init logger")
        .unwrap()
}
