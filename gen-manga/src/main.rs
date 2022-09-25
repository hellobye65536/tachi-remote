use std::{
    env::current_dir,
    fmt::Display,
    fs,
    io::{self, BufWriter, Write},
    ops::ControlFlow,
    path::PathBuf,
};

use lexopt::{Arg, Parser};
use uuid::Uuid;

const APP_NAME: &str = "gen-manga";

macro_rules! format_help {
    ($($v:tt)*) => {
        format_args!(
            concat!(
                "{app_name} ", env!("CARGO_PKG_VERSION"), "\n",
                "Generates an info.json to stdout from the provided directory\n",
                "Title will be the directory name.\n",
                "Any cover.* file as the cover, and anything else as a chapter.\n",
                "Sorts file/directory names for chapters alphabetically\n",
                "\n",
                "USAGE:\n",
                "    {app_name} [options] [path]\n",
                "\n",
                "ARGS:\n",
                "    [path]               path to the manga directory, defaults to the current working directory\n",
                "\n",
                "OPTIONS:\n",
                "    -h, --help           print help\n",
                // "    -o, --output=file    write output to file or '-' for stdout, defaults to stdout\n"
            ),
            $($v)*
        )
    };
}

#[derive(Debug)]
pub struct Args {
    path: PathBuf,
}

impl Args {
    pub fn parse_args() -> anyhow::Result<ControlFlow<(), Self>> {
        let mut args = ArgsPartial::default();
        let mut arg_index = 0usize;

        let mut parser = Parser::from_env();

        while let Some(arg) = parser.next()? {
            match arg {
                Arg::Value(v) => {
                    match arg_index {
                        0 => args.path = Some(PathBuf::from(v)),
                        _ => Err(Arg::Value(v).unexpected())?,
                    }
                    arg_index += 1;
                }
                Arg::Short('h') | Arg::Long("help") => {
                    io::stdout().write_fmt(format_help!(
                        app_name = parser.bin_name().unwrap_or(APP_NAME),
                    ))?;
                    return Ok(ControlFlow::Break(()));
                }
                arg => return Err(arg.unexpected().into()),
            }
        }

        Ok(ControlFlow::Continue(args.try_into()?))
    }
}

#[derive(Debug, Default)]
struct ArgsPartial {
    path: Option<PathBuf>,
}

impl TryFrom<ArgsPartial> for Args {
    type Error = anyhow::Error;

    fn try_from(v: ArgsPartial) -> Result<Self, Self::Error> {
        Ok(Self {
            path: v.path.map_or_else(current_dir, Ok)?,
        })
    }
}

fn main() {
    if let Err(e) = main_err() {
        eprintln!("error: {:#}", e);
        std::process::exit(1);
    }
}

fn main_err() -> anyhow::Result<()> {
    let Args { path } = match Args::parse_args()? {
        ControlFlow::Continue(v) => v,
        ControlFlow::Break(()) => return Ok(()),
    };

    let stdout = io::stdout();
    let mut write = BufWriter::new(stdout.lock());

    let dir = path.read_dir()?;

    writeln!(&mut write, "id = \"{}\"", Uuid::new_v4())?;
    if let Some(name) = path.file_name() {
        if let Some(name) = name.to_str() {
            writeln!(&mut write, "title = \"{}\"", EscapedStr(name))?;
        } else {
            eprintln!("warning: directory name isn't valid unicode, using placeholder");
            writeln!(&mut write, "title = \"<title here>\"")?;
        }
    } else {
        eprintln!("warning: couldn't get directory name, using placeholder");
        writeln!(&mut write, "title = \"<title here>\"")?;
    }

    let mut chapters = Vec::new();
    let mut cover = None;
    let mut dup_cover = false;

    for entry in dir {
        let entry = entry?;
        let name = entry.file_name();

        if name == "info.toml" {
            continue;
        }

        if let Some(name_s) = name.to_str() {
            if name_s.starts_with("cover.") && fs::metadata(entry.path())?.is_file() {
                dup_cover = cover.replace(name.into_string().unwrap()).is_some();
                continue;
            }
        }

        chapters.push(name);
    }

    if let Some(cover) = cover {
        if dup_cover {
            eprintln!("warning: duplicate covers, picking one arbitrarily");
        }

        writeln!(&mut write, "cover = \"{}\"", EscapedStr(&cover))?;
    }

    write.write_all(
        concat!(
            "status = \"unknown\"\n",
            "description = \"<description here>\"\n",
            "authors = []\n",
            "artists = []\n",
            "tags = []\n",
        )
        .as_bytes(),
    )?;

    chapters.sort_unstable();

    write.write_all("chapters = [\n".as_bytes())?;
    for ch in &chapters {
        if let Some(ch) = ch.to_str() {
            writeln!(
                &mut write,
                "    {{ path = \"{ch}\", title = \"{ch}\" }},",
                ch = EscapedStr(&ch)
            )?;
        } else {
            eprintln!(
                "warning: {app_name} does not support non-unicode chapter paths, skipping: {:?}",
                ch,
                app_name = APP_NAME,
            );
        }
    }
    write.write_all("]\n".as_bytes())?;

    Ok(())
}

struct EscapedStr<'a>(&'a str);
impl<'a> Display for EscapedStr<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use std::fmt::Write;

        for c in self.0.chars() {
            match c {
                '\x08' => f.write_str("\\b")?,
                '\t' => f.write_str("\\t")?,
                '\n' => f.write_str("\\n")?,
                '\x0c' => f.write_str("\\f")?,
                '\r' => f.write_str("\\r")?,
                '"' => f.write_str("\\\"")?,
                '\\' => f.write_str("\\\\")?,
                '\x00'..='\x1f' => write!(f, "\\u{:04x}", c as u32)?,
                '\x20'..='\x7e' => f.write_char(c)?,
                '\u{007f}'..='\u{ffff}' => write!(f, "\\u{:04x}", c as u32)?,
                _ => write!(f, "\\U{:08x}", c as u32)?,
            }
        }

        Ok(())
    }
}
