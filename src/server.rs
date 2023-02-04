use std::{
    convert::Infallible,
    fmt::{self, Debug, Display},
    fs::{self, File},
    io::{self, Read, Seek, Write},
    net::{Ipv6Addr, TcpListener},
    ops::Deref,
};

use anyhow::Context;
use bstr::ByteSlice;
use flate2::read::DeflateDecoder;
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

    let tcp = TcpListener::bind((Ipv6Addr::UNSPECIFIED, port))?;

    info!(
        "hosting server at {}, serving {} manga",
        tcp.local_addr()?,
        lib.mangas.len()
    );

    let shared = &*Box::leak(Box::new(Shared { lib }));

    let make_service =
        make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(|req| shared.serve(req))) });

    hyper::Server::from_tcp(tcp)?
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
        if Method::GET != *req.method() {
            return Err(StatusCode::METHOD_NOT_ALLOWED.into());
        }

        let mut path = req.uri().path().split('/').skip(1);

        let manga = match path.next() {
            None | Some("") => return self.serve_lib(req).await,
            Some(manga) => self.lib.mangas.get(manga).ok_or(Error::NOT_FOUND)?,
        };

        let ch = match path.next() {
            None => return self.serve_manga(req, manga).await,
            Some("cover") => {
                if path.next().is_some() {
                    return Err(Error::NOT_FOUND);
                }
                return self.serve_cover(req, manga).await;
            }
            Some(ch) => ch.parse().map_err(|_| Error::NOT_FOUND)?,
        };

        let pg = match path.next() {
            None => return Err(Error::NOT_FOUND),
            Some(pg) => pg.parse().map_err(|_| Error::NOT_FOUND)?,
        };

        match path.next() {
            None => self.serve_page(req, manga, ch, pg).await,
            Some(_) => return Err(Error::NOT_FOUND),
        }
    }

    async fn serve_lib(&'static self, req: &Request<Body>) -> Result<Response, Error> {
        self.lib.json.into_response(req.headers())
    }

    async fn serve_manga(
        &'static self,
        req: &Request<Body>,
        manga: &'static MangaEntry,
    ) -> Result<Response, Error> {
        manga.json.into_response(req.headers())
    }

    async fn serve_cover(
        &'static self,
        req: &Request<Body>,
        manga: &'static MangaEntry,
    ) -> Result<Response, Error> {
        let cover = manga.cover.as_ref().ok_or(Error::NOT_FOUND)?;

        match cover {
            Cover::File(path) => fs::read(path)
                .map(|v| Response::new(v.into()))
                .with_context(|| format!("{:?}: error opening cover", cover))
                .map_err(Into::into),
            &Cover::Page { ch, pg } => self
                .serve_page(req, manga, ch, pg)
                .await
                .map_err(|e| e.with_context(|| format!("{:?}: error opening cover", cover))),
        }
    }

    async fn serve_page(
        &'static self,
        req: &Request<Body>,
        manga: &'static MangaEntry,
        ch: usize,
        pg: usize,
    ) -> Result<Response, Error> {
        let ch = manga.chapters.get(ch).ok_or(Error::NOT_FOUND)?;

        match &ch.pages {
            Pages::None => return Err(Error::NOT_FOUND),
            Pages::Filesystem(pages) => {
                let page = pages.get(pg).ok_or(Error::NOT_FOUND)?;
                let ctx = || format!("{:?}: error opening page", page);

                Ok(Response::new(fs::read(page).with_context(ctx)?.into()))
            }
            #[cfg(feature = "zip")]
            Pages::Zip(path, pages) => {
                let page = pages.get(pg).ok_or(Error::NOT_FOUND)?;
                let ctx = || format!("{:?}: error opening page", path);

                let mut file = File::open(path).with_context(ctx)?;
                file.seek(io::SeekFrom::Start(page.data_offset))
                    .with_context(ctx)?;
                let mut file = file.take(page.compressed_size);

                let mut buf =
                    Vec::with_capacity(page.uncompressed_size.try_into().expect("usize overflow"));

                match page.method {
                    rc_zip::Method::Store => {
                        file.read_to_end(&mut buf).with_context(ctx)?;
                    }
                    rc_zip::Method::Deflate => {
                        if req
                            .headers()
                            .get(ACCEPT_ENCODING)
                            .map(|v| v.to_str().map_err(|_| Error::NOT_ACCEPTABLE))
                            .transpose()?
                            .map_or(false, |v| v.contains("deflate"))
                        {
                            file.read_to_end(&mut buf).with_context(ctx)?;
                            let mut resp = Response::new(buf.into());
                            resp.headers_mut()
                                .insert(CONTENT_ENCODING, HeaderValue::from_static("deflate"));
                            return Ok(resp);
                        }

                        DeflateDecoder::new(file)
                            .read_to_end(&mut buf)
                            .with_context(ctx)?;
                    }
                    _ => Err(anyhow::anyhow!("unsupported compression type")).with_context(ctx)?,
                }

                Ok(Response::new(buf.into()))
            }
        }
    }
}

#[derive(Debug)]
pub enum Error {
    StatusCode(StatusCode),
    Other(anyhow::Error),
}

impl From<StatusCode> for Error {
    fn from(v: StatusCode) -> Self {
        Self::StatusCode(v)
    }
}

impl From<anyhow::Error> for Error {
    fn from(v: anyhow::Error) -> Self {
        Self::Other(v)
    }
}

impl Error {
    pub const NOT_FOUND: Self = Self::StatusCode(StatusCode::NOT_FOUND);
    pub const NOT_ACCEPTABLE: Self = Self::StatusCode(StatusCode::NOT_ACCEPTABLE);

    pub fn into_response(self) -> Response<Body> {
        let mut res = Response::new(Body::empty());

        *res.status_mut() = match self {
            Self::StatusCode(status_code) => status_code,
            Self::Other(e) => {
                error!("{}", e);
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        res
    }

    pub fn with_context<T: Display + Send + Sync + 'static>(self, f: impl FnOnce() -> T) -> Self {
        match self {
            Self::Other(e) => Self::Other(e.context(f())),
            _ => self,
        }
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
        gzip.write_all(&raw).expect("Vec::write never fails");
        let gzip = gzip
            .finish()
            .expect("Vec::write never fails")
            .into_boxed_slice();
        let gzip = (gzip.len() < raw.len()).then_some(gzip);

        Self { raw, gzip }
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
            Some(v) => v.to_str().map_err(|_| Error::NOT_ACCEPTABLE)?,
            None => return Ok(json(self.raw.deref(), None)),
        };

        if let (Some(gzip), true) = (&self.gzip, accept_encoding.contains("gzip")) {
            Ok(json(gzip, Some("gzip")))
        } else {
            Ok(json(&self.raw, None))
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
