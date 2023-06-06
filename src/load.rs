use std::{
    borrow::Cow,
    collections::HashMap,
    fmt::{self, Debug},
    fs::{self, File},
    io::{self, Read},
    mem,
    path::{Path, PathBuf},
};

use anyhow::Context;
use log::error;

use serde::{de::Visitor, Deserialize, Deserializer, Serialize, Serializer};
use walkdir::WalkDir;

use crate::server::JsonBytes;

pub fn load_library<P: AsRef<Path>>(path: &[P]) -> anyhow::Result<LibraryEntry> {
    let mut walk = WalkDir::new(&path[0])
        .max_open(128)
        .follow_links(true)
        .into_iter()
        .filter_entry(|entry| entry.file_type().is_dir());

    let mut lib_buf = vec![b'['];
    let mut mangas: HashMap<String, MangaEntry> = HashMap::new();

    let mut read_buf = Vec::new();
    while let Some(entry) = walk.next() {
        let lib_buf_pos = lib_buf.len();

        let res = (|| -> anyhow::Result<()> {
            let mut path = entry?.into_path();

            if let Some(manga) = load_manga(&mut path, &mut read_buf) {
                walk.skip_current_dir();
                let mut manga =
                    manga.with_context(|| anyhow::anyhow!("{:?}: error reading manga", path))?;

                struct LibraryEntrySer<'a>(&'a Manga<'a>);
                impl<'a> Serialize for LibraryEntrySer<'a> {
                    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
                        use serde::ser::SerializeStruct;
                        let mut ser = ser.serialize_struct("LibraryEntrySer", 2)?;
                        ser.serialize_field("id", &self.0.id)?;
                        ser.serialize_field("title", &self.0.title)?;
                        ser.end()
                    }
                }
                serde_json::to_writer(&mut lib_buf, &LibraryEntrySer(&manga))?;
                lib_buf.push(b',');

                mangas.insert(
                    mem::take(&mut manga.id).into_owned(),
                    MangaEntry::new(manga)?,
                );
            }

            Ok(())
        })();

        if let Err(e) = res {
            lib_buf.truncate(lib_buf_pos);
            error!("error traversing directory: {:#}", e);
        }
    }

    if let Some(b',') = lib_buf.last() {
        lib_buf.pop();
    }
    lib_buf.push(b']');

    Ok(LibraryEntry {
        json: lib_buf.into(),
        mangas,
    })
}

fn load_manga<'a>(
    path: &mut PathBuf,
    read_buf: &'a mut Vec<u8>,
) -> Option<anyhow::Result<Manga<'a>>> {
    path.push("info.toml");
    let file = File::open(&path);
    path.pop();

    let file = match file {
        Ok(v) => v,
        Err(ref e) if matches!(e.kind(), io::ErrorKind::NotFound) => return None,
        Err(e) => return Some(Err(e.into())),
    };

    Some((|| {
        read_buf.clear();
        { file }.read_to_end(read_buf)?;

        let mut manga: Manga = toml::from_slice(read_buf)?;

        for (i, ch) in manga.chapters.iter_mut().enumerate() {
            ch.pages = load_chapter(path.join(&ch.path))
                .with_context(|| format!("{:?} (#{})", ch.path, i))?;
        }

        if let Some(Cover::File(cover)) = &mut manga.cover {
            path.push(&cover);
            *cover = mem::replace(path, PathBuf::new());
        }

        Ok(manga)
    })())
}

fn load_chapter(path: PathBuf) -> anyhow::Result<Pages> {
    if path.is_dir() {
        load_pages_dir(path)
    } else {
        load_pages_file(path)
    }
}

fn load_pages_dir(path: PathBuf) -> anyhow::Result<Pages> {
    let dir = path.read_dir()?;

    let mut pages = Vec::new();
    for entry in dir {
        let entry = entry?;
        let path = entry.path();
        if fs::metadata(&path)?.is_file() {
            pages.push(path);
        }
    }

    pages.sort_unstable();

    Ok(Pages::Filesystem(pages.into()))
}

fn load_pages_file(path: PathBuf) -> anyhow::Result<Pages> {
    let file = File::open(&path)?;
    let ext = match path.extension() {
        Some(ext) => ext
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("unknown file type: {:?}", ext))?,
        #[cfg(feature = "infer")]
        None => {
            use positioned_io::ReadAt;

            let mut magic = [0u8; 256];
            let len = file.read_at(0, &mut magic)?;
            let magic = &magic[..len];

            let extension = infer::get(magic)
                .ok_or_else(|| anyhow::anyhow!("unknown file type"))?
                .extension();

            log::info!("{:?}: inferred file type as: {:?}", path, extension);

            extension
        }
        #[cfg(not(feature = "infer"))]
        _ => anyhow::bail!("unknown file type"),
    };

    match ext {
        #[cfg(feature = "zip")]
        "zip" | "cbz" => Ok(load_pages_zip(path, file).context("error reading zip")?),
        _ => anyhow::bail!("unknown file type: {:?}", ext),
    }
}

#[cfg(feature = "zip")]
fn load_pages_zip(path: PathBuf, file: File) -> anyhow::Result<Pages> {
    use std::ops::Deref;

    use positioned_io::ReadAt;
    use rc_zip::{reader::sync::ReadZip, EntryContents};

    let zip = ReadZip::read_zip(&file)?;
    let mut entries = zip
        .deref()
        .entries()
        .filter(|entry| matches!(entry.contents(), EntryContents::File))
        .map(|entry| {
            let mut buf = [0; 4];
            let sz = file.read_at(entry.header_offset + 26, &mut buf)?;
            anyhow::ensure!(sz == 4, "read less than 4 bytes from zip");

            let name_len = u16::from_le_bytes([buf[0], buf[1]]);
            let extra_len = u16::from_le_bytes([buf[2], buf[3]]);

            Ok((
                entry.name(),
                ZipEntry {
                    method: entry.method(),
                    data_offset: entry.header_offset + 30 + name_len as u64 + extra_len as u64,
                    compressed_size: entry.compressed_size,
                    uncompressed_size: entry.uncompressed_size,
                },
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;

    entries.sort_unstable_by_key(|(v, _)| *v);

    let pages = entries.into_iter().map(|(_, v)| v).collect();

    Ok(Pages::Zip(path, pages))
}

#[derive(Debug)]
pub struct LibraryEntry {
    pub json: JsonBytes,
    pub mangas: HashMap<String, MangaEntry>,
}

#[derive(Debug)]
pub struct MangaEntry {
    pub json: JsonBytes,
    pub cover: Option<Cover>,
    pub chapters: Box<[ChapterEntry]>,
}

impl MangaEntry {
    fn new(manga: Manga) -> anyhow::Result<Self> {
        Ok(Self {
            json: serde_json::to_vec(&manga)?.into(),
            cover: manga.cover.map(Into::into),
            chapters: manga.chapters.into_iter().map(ChapterEntry::new).collect(),
        })
    }
}

#[derive(Debug)]
pub struct ChapterEntry {
    pub pages: Pages,
}

impl ChapterEntry {
    fn new(ch: Chapter) -> Self {
        Self { pages: ch.pages }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct Manga<'a> {
    #[serde(borrow)]
    #[serde(skip_serializing)]
    pub id: Cow<'a, str>,
    #[serde(borrow)]
    pub title: Cow<'a, str>,
    #[serde(skip_serializing)]
    pub cover: Option<Cover>,
    #[serde(default)]
    #[serde(skip_serializing_if = "MangaStatus::is_unknown")]
    pub status: MangaStatus,
    #[serde(default)]
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default)]
    #[serde(skip_serializing_if = "TachiyomiList::is_empty")]
    pub authors: TachiyomiList<'a>,
    #[serde(default)]
    #[serde(skip_serializing_if = "TachiyomiList::is_empty")]
    pub artists: TachiyomiList<'a>,
    #[serde(default)]
    #[serde(skip_serializing_if = "TachiyomiList::is_empty")]
    pub tags: TachiyomiList<'a>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub chapters: Vec<Chapter<'a>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Chapter<'a> {
    #[serde(borrow)]
    #[serde(skip_serializing)]
    pub path: Cow<'a, Path>,
    #[serde(borrow)]
    pub title: Cow<'a, str>,
    #[serde(default)]
    #[serde(skip_serializing_if = "is_zero")]
    pub date: u64,
    #[serde(skip_deserializing)]
    pub pages: Pages,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default, Serialize)]
#[serde(transparent)]
struct TachiyomiList<'a>(pub Cow<'a, str>);

impl<'de> TachiyomiList<'de> {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for TachiyomiList<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::{Error, SeqAccess};

        struct VisitorImpl;
        impl<'de> Visitor<'de> for VisitorImpl {
            type Value = TachiyomiList<'de>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                write!(formatter, "a string or list of strings")
            }

            fn visit_borrowed_str<E: Error>(self, v: &'de str) -> Result<Self::Value, E> {
                Ok(TachiyomiList(Cow::Borrowed(v)))
            }

            fn visit_str<E: Error>(self, v: &str) -> Result<Self::Value, E> {
                self.visit_string(v.into())
            }

            fn visit_string<E: Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(TachiyomiList(Cow::Owned(v)))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut res: String = seq.next_element()?.unwrap_or_default();

                while let Some(v) = seq.next_element()? {
                    res.push_str(", ");
                    res.push_str(v);
                }

                self.visit_string(res)
            }
        }

        d.deserialize_any(VisitorImpl)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", into = "u32")]
pub enum MangaStatus {
    Unknown = 0,
    Ongoing = 1,
    Completed = 2,
    Licensed = 3,
    PublishingFinished = 4,
    Cancelled = 5,
    OnHiatus = 6,
}

impl MangaStatus {
    /// Returns `true` if the manga status is [`Unknown`].
    ///
    /// [`Unknown`]: MangaStatus::Unknown
    #[must_use]
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }
}

impl Default for MangaStatus {
    fn default() -> Self {
        Self::Unknown
    }
}

impl From<MangaStatus> for u32 {
    fn from(v: MangaStatus) -> Self {
        v as Self
    }
}

fn is_zero(&v: &u64) -> bool {
    v == 0
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Cover {
    File(PathBuf),
    Page {
        #[serde(alias = "chapter")]
        ch: usize,
        #[serde(alias = "page")]
        pg: usize,
    },
}

#[derive(Debug)]
pub enum Pages {
    None,
    Filesystem(Box<[PathBuf]>),
    #[cfg(feature = "zip")]
    Zip(PathBuf, Box<[ZipEntry]>),
}

impl Default for Pages {
    fn default() -> Self {
        Self::None
    }
}

impl Pages {
    pub fn len(&self) -> u32 {
        match self {
            Pages::None => 0,
            Pages::Filesystem(v) => v.len(),
            Pages::Zip(.., v) => v.len(),
        }
        .try_into()
        .expect("over u32::MAX (4,294,967,295) pages")
    }
}

impl Serialize for Pages {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(self.len())
    }
}

#[cfg(feature = "zip")]
#[derive(Debug, Clone, Copy)]
pub struct ZipEntry {
    pub method: rc_zip::Method,
    pub data_offset: u64,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
}
