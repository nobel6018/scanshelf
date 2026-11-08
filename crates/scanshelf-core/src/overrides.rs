//! Mac에서 고친 내용(`edits.json`). manifest.json은 iPhone만 쓰고, Mac은 이 파일만 쓴다.
//! iPhone이 페이지를 더 찍어 manifest를 다시 보내도 Mac에서 고친 이름과 꼭짓점이 사라지지 않는다.
//!
//! @see docs/document-format.md

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{DocumentKind, Manifest, ManifestError, PageLayout, Quad, fsio};

pub const EDITS_FILE: &str = "edits.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<DocumentKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// 손으로 고친 꼭짓점. iPhone 감지값보다 우선한다
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub quads: BTreeMap<Uuid, Quad>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub layouts: BTreeMap<Uuid, PageLayout>,
    /// 자동 회전이 틀렸을 때 사람이 더 돌린 각도(시계 방향)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub rotations: BTreeMap<Uuid, u16>,
    /// Mac에서 지운 페이지. 원본은 남기고 목록에서만 뺀다
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted_pages: Vec<Uuid>,
    /// `/` 계층 태그(tags.rs 규칙으로 정리). 사람과 Mac AI가 같이 쓴다
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// 사람이 태그를 고쳤으면 true. 그 뒤로 AI 정리는 태그를 덮지 않는다(docs/organize.md)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tags_by_user: bool,
    /// 페이지마다 고른 필터. 없으면 문서 설정(processing.json defaults)을 따른다
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub filters: BTreeMap<Uuid, crate::processing::Filter>,
    /// 사람이 보고 "확인함"을 누른 페이지. Mac 처리 확인(꼭짓점, 나누기, 방향)을 더 묻지 않는다
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviewed: Vec<Uuid>,
    /// 사람이나 AI가 고른 접힌 선 위치(0~1, 왼쪽부터). 원근 보정과 회전 뒤 그림 폭 기준이다.
    /// 펼침은 하나, 3단 접지는 둘. 배치와 수가 맞지 않으면 처리가 무시한다(자동으로 찾는다)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub folds: BTreeMap<Uuid, Vec<f32>>,
    /// 접힌 선을 근처 자국에 붙이지 않고 준 위치 그대로 세로로 자를 페이지. 붙이기가 그림 경계 같은 다른 세로선에 끌려갈 때 쓴다
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exact_folds: Vec<Uuid>,
    /// 3단 접지의 면 순서. 문서에 놓일 순서대로 왼쪽부터 센 면 번호(0~2). 예: [2, 0, 1]은 오른쪽 면(표지)이 먼저
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub panel_order: BTreeMap<Uuid, Vec<u8>>,
    /// 페이지 순서를 바꿨으면 전체 순서. 여기 없는 새 페이지는 촬영 순서대로 뒤에 붙는다
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_order: Option<Vec<Uuid>>,
}

/// 처리에 넘기는 페이지별 손 지정. 비어 있으면 자동
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageHints {
    pub folds: Vec<f32>,
    /// 접힌 선을 준 위치 그대로 쓴다(자국에 붙이지 않는다)
    pub exact: bool,
    pub panel_order: Vec<u8>,
}

impl Edits {
    pub fn hints(&self, page: Uuid) -> PageHints {
        PageHints {
            folds: self.folds.get(&page).cloned().unwrap_or_default(),
            exact: self.exact_folds.contains(&page),
            panel_order: self.panel_order.get(&page).cloned().unwrap_or_default(),
        }
    }

    /// 없으면 빈 수정(고친 것 없음)
    pub fn load(document_dir: &Path) -> Result<Edits, ManifestError> {
        Ok(fsio::read_json_optional(&document_dir.join(EDITS_FILE))?.unwrap_or_default())
    }

    pub fn save(&self, document_dir: &Path) -> Result<(), ManifestError> {
        fsio::write_json(&document_dir.join(EDITS_FILE), self)
    }

    /// manifest에 Mac 수정을 덮어쓴 결과. 화면과 처리 단계는 이 값을 쓴다
    pub fn apply(&self, manifest: &Manifest) -> Manifest {
        let mut merged = manifest.clone();
        if let Some(name) = &self.name {
            merged.name = name.clone();
        }
        if let Some(kind) = self.kind {
            merged.kind = kind;
        }
        if let Some(session) = &self.session {
            merged.session = Some(session.clone());
        }
        merged.pages.retain(|page| !self.deleted_pages.contains(&page.id));
        for page in &mut merged.pages {
            if let Some(quad) = self.quads.get(&page.id) {
                page.quad = Some(*quad);
            }
            if let Some(layout) = self.layouts.get(&page.id) {
                page.layout = *layout;
            }
            if let Some(rotation) = self.rotations.get(&page.id) {
                page.extra_rotation = *rotation;
            }
        }
        if let Some(order) = &self.page_order {
            let position = |id: &Uuid| order.iter().position(|o| o == id).unwrap_or(usize::MAX);
            // 안정 정렬이라 순서에 없는 새 페이지는 촬영 순서를 유지한 채 뒤로 간다
            merged.pages.sort_by_key(|page| position(&page.id));
        }
        merged
    }
}
