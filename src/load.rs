use std::{
    fs::{self, File},
    io::{self, Read},
    mem,
    path::{Path, PathBuf},
};

use anyhow::Context;
use log::{error, info};

use positioned_io::ReadAt;
use rc_zip::StoredEntry;
use serde::{Deserialize, Serialize, Serializer};
use walkdir::WalkDir;

pub fn load_library(path: &Path, mangas: &mut Vec<Manga>, read_buf: &mut Vec<u8>) {
    let mut walk = WalkDir::new(path)
        .max_open(128)
        .follow_links(true)
        .into_iter()
        .filter_entry(|entry| entry.file_type().is_dir());
    while let Some(entry) = walk.next() {
        let entry = match entry {
            Ok(v) => v,
            Err(e) => {
                error!("error traversing directory: {}", e);
                continue;
            }
        };

        if let Some(manga) = load_manga(entry.path(), read_buf) {
            mangas.extend(manga.map_err(|e| {
                error!("{:?}: error reading manga: {:#}", entry.path(), e);
            }));
            walk.skip_current_dir();
        }
    }
}

fn load_manga(path: &Path, read_buf: &mut Vec<u8>) -> Option<anyhow::Result<Manga>> {
    let mut path = path.to_path_buf();
    path.push("info.json");

    let file = match File::open(&path) {
        Err(ref e) if matches!(e.kind(), io::ErrorKind::NotFound) => return None,
        v => v,
    };

    path.pop();

    Some((|| {
        read_buf.clear();
        file?.read_to_end(read_buf)?;

        let mut info: MangaInfo = serde_json::from_slice(read_buf)?;

        let cover = info.cover.as_ref().map(|cover| path.join(cover));

        let chapters = mem::replace(&mut info.chapters, Vec::new())
            .into_iter()
            .enumerate()
            .map(|(i, ch)| load_chapter(&path, ch, i))
            .collect::<Result<_, _>>()?;

        Ok(Manga::new(info, cover, chapters))
    })())
}

fn load_chapter(manga_path: &Path, ch: ChapterInfo, i: usize) -> anyhow::Result<Chapter> {
    let path = manga_path.join(&ch.path);

    (|| -> anyhow::Result<_> {
        let pages = if path.is_dir() {
            load_pages_dir(&path)?
        } else {
            let file = File::open(&path)?;
            load_pages_file(path, file)?
        };

        Ok(Chapter {
            title: ch.title,
            date: ch.date,
            pages,
        })
    })()
    .with_context(|| format!("{:?} (#{})", ch.path, i))
}

fn load_pages_dir(path: &Path) -> anyhow::Result<Pages> {
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

    Ok(Pages::Filesystem(pages))
}

fn load_pages_file(path: PathBuf, file: File) -> anyhow::Result<Pages> {
    let ext = match path.extension() {
        Some(ext) => ext
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("unknown file type: {:?}", ext))?,
        #[cfg(feature = "infer")]
        None => {
            let mut magic = [0u8; 256];
            let len = file.read_at(0, &mut magic)?;
            let magic = &magic[..len];

            let extension = infer::get(magic)
                .ok_or_else(|| anyhow::anyhow!("unknown file type: magic: {:?}", magic))?
                .extension();

            info!("{:?}: inferred file type as: {:?}", path, extension);

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

#[derive(Debug, Deserialize)]
pub struct MangaInfo {
    pub id: String,
    pub title: String,
    pub cover: Option<String>,
    #[serde(default)]
    pub status: MangaStatus,
    pub description: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub artists: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub chapters: Vec<ChapterInfo>,
}

#[derive(Debug, Deserialize)]
pub struct ChapterInfo {
    pub title: String,
    pub path: String,
    #[serde(default)]
    pub date: u64,
    // uploader
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

#[derive(Debug, Serialize)]
pub struct Manga {
    #[serde(skip)]
    pub id: String,
    pub title: String,
    #[serde(skip)]
    pub cover: Option<PathBuf>,
    #[serde(skip_serializing_if = "MangaStatus::is_unknown")]
    pub status: MangaStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub authors: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub artists: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tags: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub chapters: Vec<Chapter>,
}

impl Manga {
    pub fn new(info: MangaInfo, cover: Option<PathBuf>, chapters: Vec<Chapter>) -> Self {
        fn join(strs: Vec<String>) -> String {
            let mut strs = strs.into_iter();
            let mut out = strs.next().unwrap_or_else(String::new);

            for s in strs {
                out.push_str(", ");
                out.push_str(&s);
            }

            out
        }

        Self {
            id: info.id,
            title: info.title,
            cover,
            status: info.status,
            description: info.description,
            authors: join(info.authors),
            artists: join(info.artists),
            tags: join(info.tags),
            chapters,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Chapter {
    pub title: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub date: u64,
    pub pages: Pages,
}

fn is_zero(&v: &u64) -> bool {
    v == 0
}

#[derive(Debug)]
pub enum Pages {
    Filesystem(Vec<PathBuf>),
    Zip(PathBuf, Vec<MiniZipEntry>),
}

impl Pages {
    pub fn len(&self) -> u32 {
        match self {
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
