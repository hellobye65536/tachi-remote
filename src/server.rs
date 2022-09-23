use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, Read, Seek},
    net::Ipv6Addr,
    ops::Deref,
    path::PathBuf,
};

use anyhow::Context;
use futures::TryFutureExt;
use log::{info, warn};
use serde::Serializer;

use axum::{
    body::{Full, HttpBody},
    extract::Path,
};
use axum::{response::IntoResponse, routing::get, Router};
use http::{
    header::{HeaderName, CONTENT_TYPE},
    HeaderValue, StatusCode,
};
use tokio::signal::ctrl_c;
use tower_http::compression::{predicate::SizeAbove, CompressionLayer, Predicate};

use crate::load::{Chapter, Manga, Pages};

#[derive(Debug, Default)]
pub struct ServerBuilder {
    port: u16,
    // threads: Option<NonZeroUsize>,
}

impl ServerBuilder {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            ..Default::default()
        }
    }

    pub fn run(self, lib: Vec<Manga>) -> anyhow::Result<()> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("Failed to build async runtime")?
            .block_on(Shared::new(lib)?.leak().run(self.port))
    }
}

#[derive(Debug)]
struct Shared {
    lib_entry: &'static LibraryEntry,
    entries: &'static HashMap<String, MangaEntry>,
}

impl Shared {
    fn new(lib: Vec<Manga>) -> anyhow::Result<Self> {
        let lib_entry = Box::leak(Box::new(
            LibraryEntry::new(&lib).context("failed to construct entry")?,
        ));

        let entries = Box::leak(Box::new(HashMap::new()));
        for mut manga in lib {
            entries.insert(
                std::mem::replace(&mut manga.id, String::new()),
                MangaEntry::new(manga).context("failed to construct entry")?,
            );
        }

        Ok(Self { lib_entry, entries })
    }

    fn leak(self) -> &'static Self {
        Box::leak(Box::new(self))
    }

    async fn run(&'static self, port: u16) -> anyhow::Result<()> {
        let serve_v1 = Router::new()
            .route("/", get(|| self.serve_lib()))
            .route("/:manga", get(|path| self.serve_manga(path)))
            .route("/:manga/cover", get(|path| self.serve_cover(path)))
            .route("/:manga/:ch/:pg", get(|path| self.serve_page(path)));

        let serve = Router::new().nest("/v1", serve_v1).layer(
            CompressionLayer::new().compress_when(SizeAbove::new(64).and(NotForEmptyContentType)),
        );

        info!("hosting server at port {}", port);

        hyper::Server::try_bind(&(Ipv6Addr::UNSPECIFIED, port).into())?
            .serve(serve.into_make_service())
            .with_graceful_shutdown(ctrl_c().unwrap_or_else(|_| ()))
            .await?;

        Ok(())
    }

    async fn serve_lib(&'static self) -> impl IntoResponse {
        wrap_json_mime(self.lib_entry.json.deref())
    }

    async fn serve_manga(&'static self, Path(manga): Path<String>) -> impl IntoResponse {
        self.entries
            .get(&manga)
            .ok_or(StatusCode::NOT_FOUND)
            .map(|v| wrap_json_mime(v.json.deref()))
    }

    async fn serve_cover(
        &'static self,
        Path(manga): Path<String>,
    ) -> Result<impl IntoResponse, StatusCode> {
        let cover = self
            .entries
            .get(&manga)
            .and_then(|manga| manga.cover.as_ref())
            .ok_or(StatusCode::NOT_FOUND)?;

        fs::read(cover)
            .map_err(|e| {
                warn!("{:?}: error opening page: {}", cover, e);
                StatusCode::INTERNAL_SERVER_ERROR
            })
            .map(Full::from)
    }

    async fn serve_page(
        &'static self,
        Path((manga, ch, pg)): Path<(String, usize, usize)>,
    ) -> Result<impl IntoResponse, StatusCode> {
        macro_rules! try_opt {
            ($v:expr) => {
                match $v {
                    Some(v) => v,
                    None => {
                        return Err(StatusCode::NOT_FOUND);
                    }
                }
            };
        }
        macro_rules! try_res {
            ($v:expr, $msg:expr) => {
                match $v {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("{:?}: error opening page: {}", $msg, e);
                        return Err(StatusCode::INTERNAL_SERVER_ERROR);
                    }
                }
            };
        }

        let ch = try_opt!(try_opt!(self.entries.get(&manga)).chapters.get(ch));

        match &ch.pages {
            Pages::Filesystem(pages) => {
                let page = try_opt!(pages.get(pg));

                Ok(Full::from(try_res!(fs::read(page), page)).into_response())
            }
            Pages::Zip(path, pages) => {
                let page = try_opt!(pages.get(pg));

                let mut file = try_res!(File::open(path), path);
                let stored_entry = page.as_stored_entry();

                try_res!(
                    file.seek(io::SeekFrom::Start(stored_entry.header_offset)),
                    path
                );

                let mut page_reader = stored_entry.reader(|_| &file);

                let mut buf = Vec::with_capacity(stored_entry.uncompressed_size as usize);
                try_res!(page_reader.read_to_end(&mut buf), path);

                Ok(Full::from(buf).into_response())
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct NotForEmptyContentType;
impl Predicate for NotForEmptyContentType {
    fn should_compress<B: HttpBody>(&self, response: &http::Response<B>) -> bool {
        response.headers().contains_key(CONTENT_TYPE)
    }
}

fn wrap_json_mime<T>(v: T) -> ([(HeaderName, HeaderValue); 1], T) {
    (
        [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        v,
    )
}

#[derive(Debug)]
pub struct LibraryEntry {
    pub json: Vec<u8>,
}

impl LibraryEntry {
    pub fn new(mangas: &[Manga]) -> anyhow::Result<Self> {
        use serde::ser::{SerializeSeq, SerializeStruct};

        struct SerManga<'a>(&'a Manga);
        impl<'a> serde::ser::Serialize for SerManga<'a> {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                let mut s = serializer.serialize_struct("Manga", 2)?;
                s.serialize_field("id", &self.0.id)?;
                s.serialize_field("title", &self.0.title)?;
                s.end()
            }
        }

        let mut json = Vec::new();

        let mut ser = serde_json::Serializer::new(&mut json);
        let mut ser = ser.serialize_seq(Some(mangas.len()))?;
        for manga in mangas {
            ser.serialize_element(&SerManga(manga))?;
        }
        SerializeSeq::end(ser)?;

        Ok(Self { json })
    }
}

#[derive(Debug)]
pub struct MangaEntry {
    pub json: Vec<u8>,
    pub cover: Option<PathBuf>,
    pub chapters: Vec<ChapterEntry>,
}

impl MangaEntry {
    pub fn new(manga: Manga) -> anyhow::Result<Self> {
        Ok(Self {
            json: serde_json::to_vec(&manga)?,
            cover: manga.cover,
            chapters: manga.chapters.into_iter().map(ChapterEntry::new).collect(),
        })
    }
}

#[derive(Debug)]
pub struct ChapterEntry {
    pub pages: Pages,
}

impl ChapterEntry {
    fn new(chapter: Chapter) -> Self {
        Self {
            pages: chapter.pages,
        }
    }
}
