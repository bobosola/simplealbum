use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct AlbumResponse {
    pub path: String,
    pub name: String,
    pub breadcrumbs: Vec<Breadcrumb>,
    pub folders: Vec<FolderItem>,
    pub photos: Vec<PhotoItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Breadcrumb {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderItem {
    pub name: String,
    pub path: String,
    pub cover: Option<String>,
    pub count_photos: usize,
    pub count_albums: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct PhotoItem {
    pub name: String,
    #[serde(rename = "type")]
    pub media_type: String,
    pub thumb: String,
    pub width: u32,
    pub height: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetCoverRequest {
    pub image_path: String,
    pub targets: Vec<String>,
}
