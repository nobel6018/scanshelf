//! 플랫폼마다 다른 이미지 읽기와 테두리 감지. Windows를 붙일 때 여기와 `ocr/`만 바꾼다.
//!
//! @see docs/pipeline.md

use std::path::{Path, PathBuf};

use image::RgbImage;
use scanshelf_core::Quad;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::rgb_to_cgimage;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("이미지를 열지 못했다: {0}")]
    Open(PathBuf),
    #[error("이미지를 풀지 못했다: {0}")]
    Decode(PathBuf),
}

/// EXIF 회전을 적용한 RGB. `max_pixel_size`를 주면 긴 변을 그 크기 이하로 줄인다
pub fn decode_oriented(path: &Path, max_pixel_size: Option<u32>) -> Result<RgbImage, DecodeError> {
    #[cfg(target_os = "macos")]
    {
        macos::decode_oriented(path, max_pixel_size)
    }
    #[cfg(not(target_os = "macos"))]
    {
        // HEIC는 아직 못 읽는다. Windows는 WIC로 바꿀 자리
        let image = image::open(path).map_err(|_| DecodeError::Decode(path.to_path_buf()))?;
        let image = match max_pixel_size {
            Some(max) => image.thumbnail(max, max),
            None => image,
        };
        Ok(image.to_rgb8())
    }
}

/// Vision 사각형 감지 후보(문서 영역 감지가 놓쳤을 때)
pub fn detect_rectangle_quads(image: &RgbImage) -> Vec<Quad> {
    #[cfg(target_os = "macos")]
    {
        macos::detect_rectangle_quads(image)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = image;
        Vec::new()
    }
}

/// 원본 해상도에서 문서 테두리를 다시 찾는다. 못 찾으면 없음(사진 전체를 쓴다)
pub fn detect_document_quad(image: &RgbImage) -> Option<Quad> {
    #[cfg(target_os = "macos")]
    {
        macos::detect_document_quad(image)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = image;
        None
    }
}
