use std::{
    borrow::Cow,
    collections::HashMap,
    fmt::{self, Debug, Display},
    fs::{self, File},
    io::{self, Read},
    mem,
    path::{Path, PathBuf},
};

use anyhow::Context;
use bstr::ByteSlice;
use log::error;

use rc_zip::StoredEntry;
use serde::{de::Visitor, Deserialize, Deserializer, Serialize, Serializer};
use walkdir::WalkDir;

use axum::response::IntoResponse;
use http::{header::CONTENT_TYPE, HeaderValue};

pub fn load_library<P: AsRef<Path>>(path: &[P]) -> anyhow::Result<LibraryEntry> {
    let mut walk = WalkDir::new(&path[0])
        .max_open(128)
        .follow_links(true)
        .into_iter()
        .filter_entry(|entry| entry.file_type().is_dir());

    let mut lib_buf = vec![b'['];
    let mut mangas = HashMap::new();

    let mut read_buf = Vec::new();
    while let Some(entry) = walk.next() {
        let lib_buf_pos = lib_buf.len();

        let res = (|| -> anyhow::Result<()> {
            let mut path = entry?.into_path();

            if let Some(manga) = load_manga(&mut path, &mut read_buf) {
                walk.skip_current_dir();
                let manga =
                    manga.with_context(|| anyhow::anyhow!("{:?}: error reading manga", path))?;

                struct LibraryEntrySer<'a>(&'a Manga<'a>);
                impl<'a> Serialize for LibraryEntrySer<'a> {
                    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
                        use serde::ser::SerializeStruct;
                        let mut ser = ser.serialize_struct("LibraryEntrySer", 2)?;
                        ser.serialize_field("id", self.0.id)?;
                        ser.serialize_field("title", self.0.title)?;
                        ser.end()
                    }
                }
                serde_json::to_writer(&mut lib_buf, &LibraryEntrySer(&manga))?;
                lib_buf.push(b',');

                mangas.insert(manga.id.into(), MangaEntry::new(manga)?);
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
    path.push("info.json");
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
            load_chapter(&path, ch, i)?;
        }

        if let Some(cover) = &mut manga.cover {
            path.push(&cover);
            *cover = mem::replace(path, PathBuf::new());
        }

        Ok(manga)
    })())
}

fn load_chapter(manga_path: &Path, ch: &mut Chapter, i: usize) -> anyhow::Result<()> {
    let path = manga_path.join(&ch.path);

    let pages = if path.is_dir() {
        load_pages_dir(path)
    } else {
        load_pages_file(path)
    };

    ch.pages = pages.with_context(|| format!("{:?} (#{})", ch.path, i))?;

    Ok(())
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
    use rc_zip::{EntryContents, ReadZip};

    let zip = file.read_zip()?;
    let mut entries = zip
        .entries()
        .into_iter()
        .filter(|entry| matches!(entry.contents(), EntryContents::File(..)))
        .map(|entry| (entry.name(), MiniZipEntry::new(entry)))
        .collect::<Vec<_>>();

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
    pub cover: Option<Box<Path>>,
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

pub struct JsonBytes {
    raw: Box<[u8]>,
}

impl From<Vec<u8>> for JsonBytes {
    fn from(v: Vec<u8>) -> Self {
        Self { raw: v.into() }
    }
}

impl IntoResponse for &'static JsonBytes {
    fn into_response(self) -> axum::response::Response {
        let mut res = self.raw.into_response();
        res.headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        res
    }
}

impl Debug for JsonBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Debug::fmt(&self.raw.as_bstr(), f)
    }
}

impl Display for JsonBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.raw.as_bstr(), f)
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct Manga<'a> {
    #[serde(skip_serializing)]
    pub id: &'a str,
    pub title: &'a str,
    #[serde(skip_serializing)]
    pub cover: Option<PathBuf>,
    #[serde(default)]
    #[serde(skip_serializing_if = "MangaStatus::is_unknown")]
    pub status: MangaStatus,
    #[serde(default)]
    #[serde(skip_serializing_if = "TachiyomiList::is_empty")]
    pub description: TachiyomiList<'a>,
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
    #[serde(skip_serializing)]
    pub path: &'a Path,
    pub title: &'a str,
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
                Ok(TachiyomiList(v.into()))
            }

            fn visit_str<E: Error>(self, v: &str) -> Result<Self::Value, E> {
                self.visit_string(v.into())
            }

            fn visit_string<E: Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(TachiyomiList(v.into()))
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

#[derive(Debug)]
pub enum Pages {
    None,
    Filesystem(Box<[PathBuf]>),
    Zip(PathBuf, Box<[MiniZipEntry]>),
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

#[derive(Debug, Clone, Copy)]
pub struct MiniZipEntry {
    method: rc_zip::Method,
    crc32: u32,
    header_offset: u64,
    compressed_size: u64,
    uncompressed_size: u64,
    is_zip64: bool,
}

impl MiniZipEntry {
    fn new(entry: &StoredEntry) -> Self {
        Self {
            method: entry.entry.method,
            crc32: entry.crc32,
            header_offset: entry.header_offset,
            compressed_size: entry.compressed_size,
            uncompressed_size: entry.uncompressed_size,
            is_zip64: entry.is_zip64,
        }
    }

    pub fn as_stored_entry(&self) -> StoredEntry {
        use rc_zip::{Entry, Mode, Version};

        StoredEntry {
            entry: Entry {
                name: Default::default(),
                method: self.method,
                comment: Default::default(),
                modified: Default::default(),
                created: Default::default(),
                accessed: Default::default(),
            },
            crc32: self.crc32,
            header_offset: self.header_offset,
            compressed_size: self.compressed_size,
            uncompressed_size: self.uncompressed_size,
            external_attrs: Default::default(),
            creator_version: Version(0),
            reader_version: Version(0),
            flags: Default::default(),
            uid: Default::default(),
            gid: Default::default(),
            mode: Mode(0),
            extra_fields: Default::default(),
            is_zip64: self.is_zip64,
        }
    }
}
