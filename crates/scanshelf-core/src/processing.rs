//! Mac 처리 결과(`processing.json`). 원본 페이지 하나가 출력 페이지 하나(single) 또는 둘(spread)이 된다.
//!
//! @see docs/document-format.md

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ManifestError, fsio};

pub const PROCESSING_FILE: &str = "processing.json";

/// 처리 알고리즘이 바뀌면 올린다. 이보다 낮은 결과는 다시 처리 대상으로 표시한다
/// 5(2026-11-08): 사진 페이지 필터, 회전 다시 확인, 그림자 없는 펼침 나누기
/// 6(2026-11-08): 곡면 보정이 잘라 낸 페이지 둘레를 자르지 않음
pub const PIPELINE_VERSION: u32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Dewarp {
    Off,
    #[default]
    Normal,
    /// vFlat "더 펴기"에 해당하는 강한 2차 보정
    Strong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Filter {
    Original,
    /// 그림자 제거, 흰색 맞춤, 대비. 기본값
    #[default]
    Enhanced,
    Grayscale,
    BlackWhite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessOptions {
    pub dewarp: Dewarp,
    pub filter: Filter,
    pub remove_fingers: bool,
    pub remove_handwriting: bool,
}

impl Default for ProcessOptions {
    fn default() -> Self {
        Self {
            dewarp: Dewarp::Normal,
            filter: Filter::Enhanced,
            remove_fingers: true,
            remove_handwriting: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PagePart {
    Whole,
    Left,
    Right,
    /// 3단 접지의 면. 왼쪽부터 0, 1, 2. JSON은 `{"panel": 0}`
    Panel(u8),
}

impl PagePart {
    /// 출력 페이지 id: 원본 페이지 id 뒤에 `-L`, `-R`, 3단 접지는 `-P1`~`-P3`. 통째면 원본 id 그대로
    pub fn output_id(self, source: Uuid) -> String {
        match self {
            PagePart::Whole => source.to_string(),
            PagePart::Left => format!("{source}-L"),
            PagePart::Right => format!("{source}-R"),
            PagePart::Panel(index) => format!("{source}-P{}", index + 1),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputPage {
    pub id: String,
    pub source_page: Uuid,
    pub part: PagePart,
    /// 예: `pages/<id>.jpg`
    pub image: String,
    /// 예: `ocr/<id>.json`. OCR 전이면 없음
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocr: Option<String>,
    pub options: ProcessOptions,
    pub pipeline_version: u32,
    pub processed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Processing {
    /// 문서 기본 옵션. 페이지마다 다르게 처리했으면 OutputPage.options에 남는다
    pub defaults: ProcessOptions,
    /// 문서 페이지 순서대로
    pub outputs: Vec<OutputPage>,
}

impl Processing {
    /// 없으면 None(아직 처리 전)
    pub fn load(document_dir: &Path) -> Result<Option<Processing>, ManifestError> {
        fsio::read_json_optional(&document_dir.join(PROCESSING_FILE))
    }

    pub fn save(&self, document_dir: &Path) -> Result<(), ManifestError> {
        fsio::write_json(&document_dir.join(PROCESSING_FILE), self)
    }
}
