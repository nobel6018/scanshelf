//! scanshelf 문서 형식. 문서 하나는 폴더 하나이고, 그 안의 `manifest.json`이 페이지 목록과 촬영 정보를 담는다.
//!
//! iPhone은 원본 사진과 감지한 꼭짓점만 기록하고, 자르기와 보정은 Mac이 한다.
//! 원본을 지우지 않으므로 Mac에서 처리 방식을 바꾼 뒤 예전 문서도 다시 처리할 수 있다.
//!
//! @see docs/document-format.md

pub mod fsio;
pub mod ocr;
pub mod overrides;
pub mod processing;
pub mod summary;
pub mod tags;
pub mod transfer;

use std::fs;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MANIFEST_FILE: &str = "manifest.json";

/// 형식이 바뀌면 올린다. 모르는 버전은 읽지 않는다(새 iPhone 앱이 보낸 문서를 옛 Mac 앱이 망가뜨리지 않게)
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest 입출력 실패 ({path}): {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("manifest 형식 오류 ({path}): {source}")]
    Parse { path: PathBuf, source: serde_json::Error },
    #[error("지원하지 않는 schemaVersion {found} (이 앱은 {SCHEMA_VERSION}까지)")]
    UnsupportedVersion { found: u32 },
    #[error("페이지 {page_id}: {reason}")]
    InvalidPage { page_id: Uuid, reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DocumentKind {
    /// 박람회 팜플렛. Mac이 OCR 뒤 AI 정리와 노션 CSV까지 만든다
    Pamphlet,
    /// 펼친 책. 두 페이지 분리와 곡면 보정 대상
    Book,
    /// 그 밖의 낱장 문서
    Document,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PageLayout {
    /// 사진 한 장 = 페이지 한 장
    Single,
    /// 펼친 두 페이지. Mac이 가운데 접힌 선에서 나눈다
    Spread,
    /// 펼친 3단 접지 팜플렛. Mac이 접힌 선 두 개에서 세 면으로 나눈다
    Trifold,
}

/// 원본 사진 기준 정규화 좌표(0~1). 원점은 왼쪽 위, 사진의 EXIF 회전을 적용한 뒤의 방향이다
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// 문서 테두리 네 꼭짓점. iPhone 감지값이 처음 들어가고, Mac에서 손으로 고치면 덮인다
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Quad {
    pub top_left: Point,
    pub top_right: Point,
    pub bottom_right: Point,
    pub bottom_left: Point,
}

impl Quad {
    /// 마주 보는 두 변 길이의 비(짧은 쪽/긴 쪽) 하한. 거치대에서 내려다본 종이는 원근이 약해 2026-11-08 촬영 154장 중
    /// 정상 장은 모두 0.89 이상이었고, 꼭짓점을 잘못 잡은 장은 0.01~0.52였다
    pub const MIN_OPPOSITE_SIDE_RATIO: f64 = 0.8;
    /// 내각 범위(도). 정상 장은 84~96도, 잘못 잡은 장은 38~167도였다
    pub const MIN_CORNER_DEGREES: f64 = 65.0;
    pub const MAX_CORNER_DEGREES: f64 = 115.0;

    pub fn points(&self) -> [Point; 4] {
        [self.top_left, self.top_right, self.bottom_right, self.bottom_left]
    }

    /// 종이 테두리로 믿을 만한가. 볼록하고, 마주 보는 변 길이가 비슷하고, 내각이 직각에 가까워야 한다.
    /// Vision이 흰 면이나 어두운 표지의 꼭짓점을 못 찾으면 접힌 선에 꼭짓점을 붙인 삼각형, 사다리꼴을 내는데
    /// 이걸로 펴면 페이지가 찢긴 것처럼 나온다. 그런 테두리는 버리고 다시 찾거나 사진 전체를 쓴다.
    /// `aspect`는 사진의 높이/폭이다. 정규화 좌표는 가로세로 단위가 달라 길이와 각도를 재기 전에 맞춘다.
    /// iPhone `QuadGeometry.isPlausible`과 같은 기준이다. @see docs/capture.md
    pub fn is_plausible(&self, aspect: f64) -> bool {
        let p = self.points().map(|q| (q.x, q.y * aspect));
        let side = |i: usize| {
            let (a, b) = (p[i], p[(i + 1) % 4]);
            ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
        };
        let sides = [side(0), side(1), side(2), side(3)];
        if sides.iter().any(|s| *s < 1e-6) {
            return false;
        }
        let ratio = |a: f64, b: f64| a.min(b) / a.max(b);
        if ratio(sides[0], sides[2]) < Self::MIN_OPPOSITE_SIDE_RATIO
            || ratio(sides[1], sides[3]) < Self::MIN_OPPOSITE_SIDE_RATIO
        {
            return false;
        }
        let mut sign = 0.0f64;
        for i in 0..4 {
            let (prev, at, next) = (p[(i + 3) % 4], p[i], p[(i + 1) % 4]);
            let (ax, ay) = (prev.0 - at.0, prev.1 - at.1);
            let (bx, by) = (next.0 - at.0, next.1 - at.1);
            let cross = ax * by - ay * bx;
            if sign == 0.0 {
                sign = cross.signum();
            } else if cross.signum() != sign {
                return false;
            }
            let cos = (ax * bx + ay * by) / (side((i + 3) % 4) * side(i));
            let degrees = cos.clamp(-1.0, 1.0).acos().to_degrees();
            if !(Self::MIN_CORNER_DEGREES..=Self::MAX_CORNER_DEGREES).contains(&degrees) {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub id: Uuid,
    /// 문서 폴더 기준 상대 경로. 예: `raw/0b4f….heic`
    pub raw: String,
    pub captured_at: DateTime<Utc>,
    pub layout: PageLayout,
    /// 감지 실패면 없음. Mac이 원본 해상도에서 다시 감지한다
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quad: Option<Quad>,
    /// 자동 회전 뒤에 사람이 더 돌린 각도(시계 방향, 0, 90, 180, 270). Mac 수정(edits.json)에서 온다
    #[serde(default, skip_serializing_if = "is_zero")]
    pub extra_rotation: u16,
}

fn is_zero(value: &u16) -> bool {
    *value == 0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub schema_version: u32,
    pub id: Uuid,
    /// 예: `2026 데이터센터코리아_한빛로보틱스`. 팜플렛은 Mac AI가 표지에서 회사명을 추천한다
    pub name: String,
    pub kind: DocumentKind,
    /// 박람회 이름처럼 여러 문서를 묶는 촬영 세션. iPhone에서 한 번 정하면 이어지는 문서에 붙는다
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub created_at: DateTime<Utc>,
    pub pages: Vec<Page>,
    /// iPhone이 저장할 때마다 1씩 올리는 번호. Wi-Fi와 iCloud로 순서가 뒤바뀌어 도착해도 옛 manifest가 새것을 덮지 않게 한다.
    /// 옛 앱이 만든 문서에는 없고 없으면 0으로 본다. 없어도 읽을 수 있어 schemaVersion은 그대로 둔다
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

impl Manifest {
    pub fn load(document_dir: &Path) -> Result<Manifest, ManifestError> {
        let path = document_dir.join(MANIFEST_FILE);
        let bytes = fs::read(&path).map_err(|source| ManifestError::Io {
            path: path.clone(),
            source,
        })?;
        let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|source| ManifestError::Parse {
            path: path.clone(),
            source,
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// 임시 파일에 쓴 뒤 이름을 바꾼다. 전송이나 iCloud 동기화 중에 반쯤 쓴 manifest를 읽는 일을 막는다
    /// 전송이나 iCloud 동기화 중에 반쯤 쓴 manifest를 읽지 않도록 원자적으로 쓴다
    pub fn save(&self, document_dir: &Path) -> Result<(), ManifestError> {
        self.validate()?;
        fsio::write_json(&document_dir.join(MANIFEST_FILE), self)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version > SCHEMA_VERSION {
            return Err(ManifestError::UnsupportedVersion {
                found: self.schema_version,
            });
        }
        for page in &self.pages {
            validate_page(page)?;
        }
        Ok(())
    }
}

fn validate_page(page: &Page) -> Result<(), ManifestError> {
    let invalid = |reason: &str| ManifestError::InvalidPage {
        page_id: page.id,
        reason: reason.to_string(),
    };
    // 다른 기기가 보낸 파일이라 경로를 믿지 않는다. 문서 폴더 밖을 가리키면 거부한다
    let raw = Path::new(&page.raw);
    let escapes = raw
        .components()
        .any(|component| !matches!(component, Component::Normal(_)));
    if page.raw.is_empty() || escapes {
        return Err(invalid("raw 경로는 문서 폴더 안의 상대 경로여야 한다"));
    }
    if page.extra_rotation >= 360 || !page.extra_rotation.is_multiple_of(90) {
        return Err(invalid("회전은 0, 90, 180, 270도만 된다"));
    }
    if let Some(quad) = page.quad {
        let in_range = quad
            .points()
            .iter()
            .all(|p| (0.0..=1.0).contains(&p.x) && (0.0..=1.0).contains(&p.y));
        if !in_range {
            return Err(invalid("꼭짓점은 0~1 정규화 좌표여야 한다"));
        }
    }
    Ok(())
}
