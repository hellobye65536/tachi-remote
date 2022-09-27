use std::{
    convert::Infallible,
    fmt::{self, Debug, Display},
    fs::{self, File},
    io::{self, Read, Seek, Write},
    net::Ipv6Addr,
    ops::Deref,
};

use anyhow::Context;
use bstr::ByteSlice;
use futures::TryFutureExt;
use log::{error, info};

use http::{
    header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_TYPE},
    HeaderMap, HeaderValue, Method, Request, StatusCode,
};
use hyper::{
    service::{make_service_fn, service_fn},
    Body,
};
use tokio::signal::ctrl_c;

use crate::load::{Cover, LibraryEntry, MangaEntry, Pages};

type Response<T = Body> = http::Response<T>;

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

    pub fn run(self, lib: LibraryEntry) -> anyhow::Result<()> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("Failed to build async runtime")?
            .block_on(run_server(self, lib))
    }
}

async fn run_server(builder: ServerBuilder, lib: LibraryEntry) -> anyhow::Result<()> {
    let ServerBuilder { port } = builder;

    info!(
        "hosting server at port {}, serving {} manga",
        port,
        lib.mangas.len()
    );

    let shared = &*Box::leak(Box::new(Shared { lib }));

    // let serve = Router::new()
    //     .route("/", get(serve_lib))
    //     .route("/:manga", get(serve_manga))
    //     .route("/:manga/cover", get(serve_cover))
    //     .route("/:manga/:ch/:pg", get(serve_page))
    //     .layer(Extension(lib));

    let make_service = make_service_fn(|_conn| {
        ();
        async { Ok::<_, Infallible>(service_fn(|req| shared.serve(req))) }
    });

    hyper::Server::try_bind(&(Ipv6Addr::UNSPECIFIED, port).into())?
        .serve(make_service)
        .with_graceful_shutdown(ctrl_c().unwrap_or_else(|_| ()))
        .await?;

    Ok(())
}

struct Shared {
    lib: LibraryEntry,
}

impl Shared {
    async fn serve(&'static self, req: Request<Body>) -> Result<Response<Body>, Infallible> {
        Ok(self.route(&req).await.unwrap_or_else(Error::into_response))
    }

    async fn route(&'static self, req: &Request<Body>) -> Result<Response<Body>, Error> {
        let mut path = req.uri().path().split('/').skip(1);

        let manga = match path.next() {
            None => return self.serve_lib(req).await,
            Some(manga) => self.lib.mangas.get(manga).ok_or(Error::NotFound)?,
        };

        let ch = match path.next() {
            None => return self.serve_manga(req, manga).await,
            Some("cover") => return self.serve_cover(req, manga).await,
            Some(ch) => ch.parse().map_err(|_| Error::NotFound)?,
        };

        let pg = match path.next() {
            None => return Err(Error::NotFound),
            Some(pg) => pg.parse().map_err(|_| Error::NotFound)?,
        };

        match path.next() {
            None => self.serve_page(req, manga, ch, pg).await,
            Some(_) => return Err(Error::NotFound),
        }
    }

    async fn serve_lib(&'static self, req: &Request<Body>) -> Result<Response, Error> {
        if !matches!(req.method(), &Method::GET) {
            return Err(Error::MethodNotAllowed);
        }

        self.lib.json.into_response(req.headers())
    }

    async fn serve_manga(
        &'static self,
        req: &Request<Body>,
        manga: &'static MangaEntry,
    ) -> Result<Response, Error> {
        if !matches!(req.method(), &Method::GET) {
            return Err(Error::MethodNotAllowed);
        }

        manga.json.into_response(req.headers())
    }

    async fn serve_cover(
        &'static self,
        req: &Request<Body>,
        manga: &'static MangaEntry,
    ) -> Result<Response, Error> {
        if !matches!(req.method(), &Method::GET) {
            return Err(Error::MethodNotAllowed);
        }

        let cover = manga.cover.as_ref().ok_or(Error::NotFound)?;

        match cover {
            Cover::File(path) => Ok(Response::new(
                fs::read(path)
                    .with_context(|| format!("{:?}: error opening cover", cover))?
                    .into(),
            )),
            &Cover::Page { ch, pg } => self.serve_page(req, manga, ch, pg).await,
        }
    }

    async fn serve_page(
        &'static self,
        req: &Request<Body>,
        manga: &'static MangaEntry,
        ch: usize,
        pg: usize,
    ) -> Result<Response, Error> {
        if !matches!(req.method(), &Method::GET) {
            return Err(Error::MethodNotAllowed);
        }

        let ch = manga.chapters.get(ch).ok_or(Error::NotFound)?;

        match &ch.pages {
            Pages::None => return Err(Error::NotFound),
            Pages::Filesystem(pages) => {
                let page = pages.get(pg).ok_or(Error::NotFound)?;
                let ctx = || format!("{:?}: error opening page", page);

                Ok(Response::new(fs::read(page).with_context(ctx)?.into()))
            }
            Pages::Zip(path, pages) => {
                let page = pages.get(pg).ok_or(Error::NotFound)?;
                let ctx = || format!("{:?}: error opening page", path);

                let mut file = File::open(path).with_context(ctx)?;
                let stored_entry = page.as_stored_entry();

                file.seek(io::SeekFrom::Start(stored_entry.header_offset))
                    .with_context(ctx)?;

                let mut page_reader = stored_entry.reader(|_| &file);

                let mut buf = Vec::with_capacity(stored_entry.uncompressed_size as usize);
                page_reader.read_to_end(&mut buf).with_context(ctx)?;

                Ok(Response::new(buf.into()))
            }
        }
    }
}

// #[derive(Debug, Clone, Copy)]
// struct NotForEmptyContentType;
// impl Predicate for NotForEmptyContentType {
//     fn should_compress<B: HttpBody>(&self, response: &http::Response<B>) -> bool {
//         response.headers().contains_key(CONTENT_TYPE)
//     }
// }

#[derive(Debug)]
pub enum Error {
    NotFound,
    MethodNotAllowed,
    NotAcceptable,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for Error {
    fn from(v: anyhow::Error) -> Self {
        Self::Other(v)
    }
}

impl Error {
    pub fn into_response(self) -> Response<Body> {
        let mut res = Response::new(Body::empty());

        *res.status_mut() = match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::NotAcceptable => StatusCode::NOT_ACCEPTABLE,
            Self::Other(e) => {
                error!("{}", e);
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        res
    }
}

pub struct JsonBytes {
    raw: Box<[u8]>,
    gzip: Option<Box<[u8]>>,
}

impl JsonBytes {
    pub fn new(raw: Box<[u8]>) -> Self {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        if raw.len() <= 64 {
            return Self { raw, gzip: None };
        }

        let gzip = Vec::new();
        let mut gzip = GzEncoder::new(gzip, Compression::best());
        gzip.write_all(&raw).unwrap();
        let gzip = gzip.finish().unwrap().into();

        Self {
            raw,
            gzip: Some(gzip),
        }
    }

    pub fn into_response(&'static self, headers: &HeaderMap) -> Result<Response, Error> {
        fn json(v: &'static [u8], enc: Option<&'static str>) -> Response {
            let mut res = Response::new(v.into());
            let headers = res.headers_mut();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            if let Some(enc) = enc {
                headers.insert(CONTENT_ENCODING, HeaderValue::from_static(enc));
            }
            res
        }

        let accept_encoding = match headers.get(ACCEPT_ENCODING) {
            Some(v) => v.to_str().map_err(|_| Error::NotAcceptable)?,
            None => return Ok(json(self.raw.deref(), None)),
        };

        if self.gzip.is_some() && accept_encoding.contains("gzip") {
            Ok(json(self.gzip.as_ref().unwrap(), Some("gzip")))
        } else {
            Ok(json(self.raw.deref(), None))
        }
    }
}

impl From<Vec<u8>> for JsonBytes {
    fn from(v: Vec<u8>) -> Self {
        Self::new(v.into())
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
