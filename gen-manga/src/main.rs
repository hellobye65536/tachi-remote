use std::{
    env::current_dir,
    fmt::Display,
    fs::File,
    io::{self, BufRead, BufReader, BufWriter, Write},
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
                "Prints a generated info.toml to stdout using the provided chapters.\n",
                "Title will be the current directory name.\n",
                "\n",
                "USAGE:\n",
                "    {app_name} [options] [chapters...]\n",
                "\n",
                "ARGS:\n",
                "    [chapters...]              path to chapters\n",
                "\n",
                "OPTIONS:\n",
                "    -h, --help                 print help\n",
                "    -c, --cover <path>         use path as cover instead of the first page of the first chapter\n",
                "    -t, --titles <file>        use lines from file for chapter titles, can be passed multiple times\n",
            ),
            $($v)*
        )
    };
}

#[derive(Debug)]
pub struct Args {
    chapters: Vec<PathBuf>,
    titles: Vec<PathBuf>,
    cover: Option<PathBuf>,
}

impl Args {
    pub fn parse_args() -> Result<Option<Self>, lexopt::Error> {
        #[derive(Debug, Default)]
        struct ArgsPartial {
            chapters: Vec<PathBuf>,
            titles: Vec<PathBuf>,
            cover: Option<PathBuf>,
        }

        let mut args = ArgsPartial::default();
        let mut arg_index = 0usize;

        let mut parser = Parser::from_env();

        while let Some(arg) = parser.next()? {
            match arg {
                Arg::Value(v) => {
                    match arg_index {
                        0.. => args.chapters.push(v.into()),
                        _ => return Err(Arg::Value(v).unexpected()),
                    }
                    arg_index += 1;
                }
                Arg::Short('h') | Arg::Long("help") => {
                    io::stdout()
                        .write_fmt(format_help!(
                            app_name = parser.bin_name().unwrap_or(APP_NAME),
                        ))
                        .map_err(|e| lexopt::Error::Custom(e.into()))?;
                    return Ok(None);
                }
                Arg::Short('c') | Arg::Long("cover") => {
                    if args.cover.replace(parser.value()?.into()).is_some() {
                        return Err("duplicate option 'cover'".into());
                    }
                }
                Arg::Short('t') | Arg::Long("titles") => args.titles.push(parser.value()?.into()),
                arg => return Err(arg.unexpected()),
            }
        }

        Ok(Some(Args {
            chapters: args.chapters,
            titles: args.titles,
            cover: args.cover,
        }))
    }
}

enum ResultIterator<T, E> {
    Done,
    Iter(T),
    Err(E),
}

impl<T: Iterator<Item = Result<V, E>>, V, E> Iterator for ResultIterator<T, E> {
    type Item = Result<V, E>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Self::Iter(iter) = self {
            match iter.next() {
                None => *self = Self::Done,
                v => return v,
            }
        }

        match std::mem::replace(self, Self::Done) {
            ResultIterator::Done => None,
            ResultIterator::Iter(_) => unreachable!(),
            ResultIterator::Err(e) => Some(Err(e)),
        }
    }
}

impl<T, E> From<Result<T, E>> for ResultIterator<T, E> {
    fn from(v: Result<T, E>) -> Self {
        match v {
            Ok(v) => Self::Iter(v),
            Err(e) => Self::Err(e),
        }
    }
}

fn main() {
    if let Err(e) = try_main() {
        eprintln!("error: {:#}", e);
        std::process::exit(1);
    }
}

fn try_main() -> anyhow::Result<()> {
    let Some(Args {
        chapters,
        titles,
        cover,
    }) = Args::parse_args()? else { return Ok(()) };

    let stdout = io::stdout();
    let mut write = BufWriter::new(stdout.lock());

    writeln!(&mut write, "id = \"{}\"", Uuid::new_v4())?;
    'title: {
        match current_dir()
            .as_ref()
            .map(|v| v.file_name().map(|v| v.to_str()))
        {
            Ok(Some(Some(title))) => {
                writeln!(&mut write, "title = \"{}\"", EscapedStr(title))?;
                break 'title;
            }
            Ok(Some(None)) => {
                eprintln!("warning: directory name isn't valid unicode, using placeholder")
            }
            Ok(None) => {
                eprintln!("warning: couldn't get directory name, using placeholder");
            }
            Err(e) => {
                eprintln!(
                    "warning: couldn't get directory name: {}, using placeholder",
                    e
                );
            }
        }
        writeln!(&mut write, "title = \"<title here>\"")?;
    }

    'cover: {
        match cover.as_ref().map(|v| v.to_str()) {
            Some(Some(cover)) => {
                writeln!(&mut write, "cover = \"{}\"", EscapedStr(&cover))?;
                break 'cover;
            }
            Some(None) => eprintln!("warning: cover path isn't valid unicode, using default"),
            None => (),
        }
        write.write_all("cover = { ch = 0, pg = 0 }\n".as_bytes())?;
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

    let mut titles = titles
        .into_iter()
        .flat_map(|v| ResultIterator::from(File::open(v).map(|v| BufReader::new(v).lines())));

    write.write_all("chapters = [\n".as_bytes())?;
    for ch in &chapters {
        let title = titles.next();

        if let Some(ch) = ch.to_str() {
            writeln!(
                &mut write,
                "    {{ path = \"{}\", title = \"{}\" }},",
                EscapedStr(ch),
                EscapedStr(title.transpose()?.as_deref().unwrap_or(ch))
            )?;
        } else {
            eprintln!(
                "warning: non-unicode chapter paths are not supported, skipping: {:?}",
                ch
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
