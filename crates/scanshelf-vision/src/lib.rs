//! scanshelf 이미지 처리. 입력과 출력은 `image::RgbImage`이고 플랫폼 API에 기대지 않는다(Windows 확장 대비).
//!
//! 처리 순서는 `pipeline`에 있다: 두 페이지 분리, 손가락 지우기, 곡면 보정, 필기 지우기, 조명 보정.
//!
//! @see docs/vision.md

pub mod dewarp;
pub mod filter;
pub mod fingers;
pub mod fold;
pub mod handwriting;
pub mod inpaint;
pub mod models;
pub mod perspective;
pub mod pipeline;
pub mod split;
pub mod util;

pub use dewarp::{DewarpResult, Framing, dewarp};
pub use filter::apply_filter;
pub use fingers::{detect_fingers, remove_fingers};
pub use fold::{Fold, Folds, Trifold, cut_at, find_folds, snap_folds, split_trifold};
pub use handwriting::{detect_colored_ink, remove_handwriting};
pub use inpaint::{InpaintEngine, inpaint};
pub use models::{Backend, MODELS, ModelId, ModelSpec, Models};
pub use pipeline::{PageReport, ProcessedPage, SourceReport, process_page, process_source};
pub use split::{Gutter, Split, find_gutter, split_spread};

#[derive(Debug, thiserror::Error)]
pub enum VisionError {
    #[error("모델 오류: {0}")]
    Model(String),
    #[error("이미지가 너무 작습니다: {0}x{1}")]
    TooSmall(u32, u32),
    #[error("마스크 크기가 이미지와 다릅니다")]
    MaskSize,
}
