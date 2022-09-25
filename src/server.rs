use std::{
    fs::{self, File},
    io::{self, Read, Seek},
    net::Ipv6Addr,
};

use anyhow::Context;
use futures::TryFutureExt;
use log::{info, warn};

use axum::{body::Full, extract::Path, response::Response, Extension};
use axum::{response::IntoResponse, routing::get, Router};
use http::StatusCode;
use tokio::signal::ctrl_c;

use crate::load::{Cover, JsonBytes, LibraryEntry, Pages};

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

    info!("hosting server at port {}", port);

    let lib = &*Box::leak(Box::new(lib));

    let serve_v1 = Router::new()
        .route("/", get(serve_lib))
        .route("/:manga", get(serve_manga))
        .route("/:manga/cover", get(serve_cover))
        .route("/:manga/:ch/:pg", get(serve_page));

    let serve = Router::new().nest("/v1", serve_v1).layer(Extension(lib));
    // .layer(
    //     CompressionLayer::new().compress_when(SizeAbove::new(64).and(NotForEmptyContentType)),
    // )

    hyper::Server::try_bind(&(Ipv6Addr::UNSPECIFIED, port).into())?
        .serve(serve.into_make_service())
        .with_graceful_shutdown(ctrl_c().unwrap_or_else(|_| ()))
        .await?;

    Ok(())
}

async fn serve_lib(Extension(lib): Extension<&'static LibraryEntry>) -> &'static JsonBytes {
    &lib.json
}

async fn serve_manga(
    Extension(lib): Extension<&'static LibraryEntry>,
    Path(manga): Path<String>,
) -> Result<&'static JsonBytes, StatusCode> {
    lib.mangas
        .get(&manga)
        .map(|v| &v.json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn serve_cover(
    Extension(lib): Extension<&'static LibraryEntry>,
    Path(manga): Path<String>,
) -> Result<Response, StatusCode> {
    let cover = lib
        .mangas
        .get(&manga)
        .and_then(|manga| manga.cover.as_ref())
        .ok_or(StatusCode::NOT_FOUND)?;

    match cover {
        Cover::File(path) => fs::read(path)
            .map_err(|e| {
                warn!("{:?}: error opening cover: {}", cover, e);
                StatusCode::INTERNAL_SERVER_ERROR
            })
            .map(|v| Full::from(v).into_response()),
        &Cover::Page { ch, pg } => serve_page(Extension(lib), Path((manga, ch, pg)))
            .await
            .map(IntoResponse::into_response),
    }
}

async fn serve_page(
    Extension(lib): Extension<&'static LibraryEntry>,
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

    let ch = try_opt!(try_opt!(lib.mangas.get(&manga)).chapters.get(ch));

    match &ch.pages {
        Pages::None => return Err(StatusCode::NOT_FOUND),
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

// #[derive(Debug, Clone, Copy)]
// struct NotForEmptyContentType;
// impl Predicate for NotForEmptyContentType {
//     fn should_compress<B: HttpBody>(&self, response: &http::Response<B>) -> bool {
//         response.headers().contains_key(CONTENT_TYPE)
//     }
// }
