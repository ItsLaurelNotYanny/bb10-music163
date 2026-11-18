//! Catalogue DTOs returned by NetEase search and playlist operations.

use provider_model::{ProviderId, TrackRef};
use serde::{Deserialize, Serialize};

/// Sanitized track metadata item suitable for search results, playlist listings, and queues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackSummary {
    pub id: u64,
    pub track_ref: TrackRef,
    pub name: String,
    pub singer: String,
    pub album: String,
    pub duration_ms: u64,
    #[serde(default)]
    pub pic_url: String,
}

impl TrackSummary {
    pub fn from_ncm(song: ncm_api::SongInfo) -> Self {
        Self {
            id: song.id,
            track_ref: TrackRef::new(ProviderId::Netease, song.id.to_string()),
            name: song.name,
            singer: song.singer,
            album: song.album,
            duration_ms: song.duration,
            pic_url: song.pic_url,
        }
    }
}

/// Sanitized playlist summary item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistSummary {
    pub id: u64,
    pub name: String,
    pub author: String,
    pub special_type: u32,
    #[serde(default)]
    pub cover_img_url: String,
}

impl PlaylistSummary {
    pub fn from_ncm(list: ncm_api::SongList) -> Self {
        Self {
            id: list.id,
            name: list.name,
            author: list.author,
            special_type: list.special_type,
            cover_img_url: list.cover_img_url,
        }
    }
}

/// Playlist metadata paired with its resolved track summaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistDetailSummary {
    pub id: u64,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub cover_img_url: String,
    pub songs: Vec<TrackSummary>,
}

impl PlaylistDetailSummary {
    pub fn from_ncm(detail: ncm_api::PlayListDetail) -> Self {
        Self {
            id: detail.id,
            name: detail.name,
            description: detail.description,
            cover_img_url: detail.cover_img_url,
            songs: detail
                .songs
                .into_iter()
                .map(TrackSummary::from_ncm)
                .collect(),
        }
    }
}
