//! A fixed, network-free library for host tests and for running the simulator without addons:
//! the Blender open movies (CC-BY) with their public sample-video URLs, and one synthetic series.

use crate::shim_backend::{Backend, CatalogInfo};
use plx_stremio::model::{Meta, MetaPreview, Stream, Subtitle, Video};
use std::time::Duration;

const GTV: &str = "https://commondatastorage.googleapis.com/gtv-videos-bucket/sample";

struct Film {
    id: &'static str,
    name: &'static str,
    year: &'static str,
    file: &'static str,
    runtime: &'static str,
    genre: &'static str,
    summary: &'static str,
}

const FILMS: &[Film] = &[
    Film { id: "tt1254207", name: "Big Buck Bunny", year: "2008", file: "BigBuckBunny.mp4", runtime: "10 min", genre: "Animation",
        summary: "A giant rabbit takes revenge on three bullying rodents." },
    Film { id: "tt1727587", name: "Sintel", year: "2010", file: "Sintel.mp4", runtime: "15 min", genre: "Fantasy",
        summary: "A lonely young woman searches for the baby dragon she befriended." },
    Film { id: "tt2285752", name: "Tears of Steel", year: "2012", file: "TearsOfSteel.mp4", runtime: "12 min", genre: "Science Fiction",
        summary: "Warriors and scientists gather in Amsterdam to save the world from destructive robots." },
    Film { id: "tt0807840", name: "Elephants Dream", year: "2006", file: "ElephantsDream.mp4", runtime: "11 min", genre: "Animation",
        summary: "Two characters explore the endless corridors of a strange machine." },
];

pub const FAKE_SERIES_ID: &str = "fake:series:1";

pub struct FakeBackend;

fn preview(f: &Film) -> MetaPreview {
    MetaPreview {
        id: f.id.into(),
        type_: "movie".into(),
        name: f.name.into(),
        poster: Some(format!("https://images.metahub.space/poster/medium/{}/img", f.id)),
        background: Some(format!("https://images.metahub.space/background/medium/{}/img", f.id)),
        logo: Some(format!("https://images.metahub.space/logo/medium/{}/img", f.id)),
        description: Some(f.summary.into()),
        release_info: Some(f.year.into()),
        imdb_rating: Some("7.0".into()),
        ratings: serde_json::Value::Null, mdb_list_ratings: serde_json::Value::Null, rotten_tomatoes_rating: None,
        genres: vec![f.genre.into()],
    }
}

fn series_preview() -> MetaPreview {
    MetaPreview {
        id: FAKE_SERIES_ID.into(),
        type_: "series".into(),
        name: "Open Movie Anthology".into(),
        poster: Some("https://images.metahub.space/poster/medium/tt1727587/img".into()),
        background: Some("https://images.metahub.space/background/medium/tt2285752/img".into()),
        logo: None,
        description: Some("The Blender open movies, as a series.".into()),
        release_info: Some("2006-2012".into()),
        imdb_rating: Some("8.1".into()),
        ratings: serde_json::Value::Null, mdb_list_ratings: serde_json::Value::Null, rotten_tomatoes_rating: None,
        genres: vec!["Animation".into()],
    }
}

fn episodes() -> Vec<Video> {
    let mut v = Vec::new();
    for s in 1..=2u32 {
        for e in 1..=FILMS.len() as u32 {
            let f = &FILMS[(e as usize - 1 + s as usize - 1) % FILMS.len()];
            v.push(Video {
                id: format!("{FAKE_SERIES_ID}:{s}:{e}"),
                title: Some(f.name.into()),
                season: Some(s),
                episode: Some(e),
                released: Some(format!("20{:02}-0{}-0{}T00:00:00.000Z", 10 + s, e, e)),
                thumbnail: Some(format!("https://images.metahub.space/background/medium/{}/img", f.id)),
                overview: Some(f.summary.into()),
                ..Default::default()
            });
        }
    }
    v
}

fn film_of(id: &str) -> Option<&'static Film> {
    if let Some(rest) = id.strip_prefix(&format!("{FAKE_SERIES_ID}:")) {
        let mut it = rest.split(':').filter_map(|x| x.parse::<usize>().ok());
        let (s, e) = (it.next()?, it.next()?);
        return Some(&FILMS[(e - 1 + s - 1) % FILMS.len()]);
    }
    FILMS.iter().find(|f| f.id == id)
}

impl Backend for FakeBackend {
    fn catalogs(&self) -> Vec<CatalogInfo> {
        vec![
            CatalogInfo { key: "fake/movie/top".into(), title: "Popular Movies".into(), type_: "movie".into(),
                genres: vec!["Animation".into(), "Fantasy".into(), "Science Fiction".into()] },
            CatalogInfo { key: "fake/series/top".into(), title: "Popular Series".into(), type_: "series".into(), genres: vec![] },
        ]
    }
    fn catalog_page(&self, key: &str, skip: u32, genre: Option<&str>) -> Result<Vec<MetaPreview>, ()> {
        if skip > 0 {
            return Ok(Vec::new());
        }
        match key {
            "fake/movie/top" => Ok(FILMS.iter().filter(|f| genre.map_or(true, |g| f.genre == g)).map(preview).collect()),
            "fake/series/top" => Ok(vec![series_preview()]),
            _ => Err(()),
        }
    }
    fn search(&self, query: &str) -> Vec<(CatalogInfo, Vec<MetaPreview>)> {
        let q = query.to_lowercase();
        let cats = self.catalogs();
        let movies = FILMS.iter().filter(|f| f.name.to_lowercase().contains(&q)).map(preview).collect();
        let series = [series_preview()].into_iter().filter(|s| s.name.to_lowercase().contains(&q)).collect();
        vec![(cats[0].clone(), movies), (cats[1].clone(), series)]
    }
    fn meta(&self, type_: &str, id: &str) -> Option<Meta> {
        if type_ == "series" && id == FAKE_SERIES_ID {
            let p = series_preview();
            return Some(Meta {
                id: p.id, type_: p.type_, name: p.name, poster: p.poster, background: p.background,
                description: p.description, release_info: p.release_info, imdb_rating: p.imdb_rating,
                runtime: Some("12 min".into()), genres: p.genres, cast: vec!["Blender Studio".into()],
                videos: episodes(), ..Default::default()
            });
        }
        let f = FILMS.iter().find(|f| f.id == id)?;
        let p = preview(f);
        Some(Meta {
            id: p.id, type_: p.type_, name: p.name, poster: p.poster, background: p.background, logo: p.logo,
            description: p.description, release_info: p.release_info, imdb_rating: p.imdb_rating,
            released: Some(format!("{}-01-01T00:00:00.000Z", f.year)), runtime: Some(f.runtime.into()),
            genres: p.genres, cast: vec!["Blender Studio".into()], director: vec!["Blender Foundation".into()],
            ..Default::default()
        })
    }
    fn streams(&self, _type_: &str, id: &str, _deadline: Duration, on_batch: &mut dyn FnMut(&[Stream])) -> Vec<Stream> {
        let Some(f) = film_of(id) else { return Vec::new() };
        let out = vec![
            Stream {
                url: Some(format!("{GTV}/{}", f.file)),
                name: Some("Fake\n1080p".into()),
                description: Some(format!("{}.1080p.WEB-DL.H264.AAC.mp4\n💾 150 MB", f.name.replace(' ', "."))),
                addon_name: "Fake".into(),
                addon_id: "fake".into(),
                ..Default::default()
            },
            Stream {
                info_hash: Some("0123456789abcdef0123456789abcdef01234567".into()),
                file_idx: Some(0),
                name: Some("Fake\n4k DV".into()),
                description: Some(format!("{}.2160p.BluRay.REMUX.DV.HDR.HEVC.TrueHD.Atmos.mkv\n👤 12 💾 58.2 GB", f.name.replace(' ', "."))),
                addon_name: "Fake".into(),
                addon_id: "fake".into(),
                ..Default::default()
            },
        ];
        on_batch(&out);
        out
    }
    fn subtitles(&self, _type_: &str, _id: &str) -> Vec<Subtitle> {
        Vec::new()
    }
}
