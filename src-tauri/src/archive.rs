//! iCloud 영구 보관. iPhone에서 온 문서의 원본을 `iCloud Drive/scanshelf/archive/<문서 id>/`에 남긴다.
//!
//! inbox는 운반용이라 Mac이 가져가면 비운다. 그러면 Mac 보관함이 유일한 사본이 되고, iPhone 원본도 기간이 지나면
//! 지워진다. 2026-11-08 사용자 결정으로 Mac이 받은 원본을 iCloud에도 계속 둔다.
//!
//! 쓰는 쪽은 Mac뿐이다. 받은 경로(Wi-Fi, iCloud)와 상관없이 보관함에 들어온 원본을 같은 모양으로 맞춘다.
//! - iCloud로 온 원본은 inbox에서 archive로 이름만 바꾼다. 같은 iCloud Drive 안이라 다시 올리지 않는다
//! - Wi-Fi로 온 원본이나 이 기능 전에 받은 문서는 보관함에서 복사한다
//! - manifest는 원본을 다 넣은 뒤 마지막에 쓴다. iPhone은 manifest의 원본이 archive에 다 보이면 "iCloud 보관"으로 본다
//!
//! 지우지 않는다. iPhone에서 지운 장, 다시 찍어 버린 장의 원본도 남는다(manifest만 지금 판). Mac 보관함에서 문서를
//! 지워도 archive는 남는다. 정리는 사람이 Finder에서 한다.
//!
//! 어느 문서를 보관할지: 받은 기록(`arrivals.json`)이 있거나 원본에 HEIC가 있는 문서. iPhone은 HEIC로 찍고,
//! Mac 가져오기(⌘O)는 PDF와 그림을 JPEG, PNG로 넣는다. 사용자가 이미 가진 파일을 iCloud에 또 올리지 않으려는 구분이다.
//!
//! @see docs/transfer.md

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use scanshelf_core::fsio::{read_json_optional, write_atomic, write_json};
use scanshelf_core::{MANIFEST_FILE, Manifest};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::store::Store;

const ARCHIVE_DIR: &str = "archive";
/// 문서 폴더 안의 받은 기록. Mac만 쓰고 iPhone으로 가지 않는다
pub const ARRIVALS_FILE: &str = "arrivals.json";
/// 받은 기록은 최근 것만 남긴다. 이어 찍기로 같은 문서를 수십 번 받을 수 있다
const MAX_ARRIVALS: usize = 50;

/// inbox 옆의 archive 폴더
pub fn archive_root(inbox: &Path) -> PathBuf {
    inbox.with_file_name(ARCHIVE_DIR)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Route {
    Wifi,
    Icloud,
}

/// 한 번 받은 일
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Arrival {
    pub route: Route,
    pub at: DateTime<Utc>,
    /// 이번에 새로 들어온 원본 수. 0이면 이미 있던 문서의 manifest만 바뀌었다(이름, 순서, 지운 장)
    pub added: usize,
}

/// 받은 기록을 덧붙인다. 실패해도 받기는 끝났으니 로그만 남긴다
pub fn record_arrival(document_dir: &Path, route: Route, added: usize) {
    let path = document_dir.join(ARRIVALS_FILE);
    let mut arrivals: Vec<Arrival> = read_json_optional(&path).ok().flatten().unwrap_or_default();
    arrivals.push(Arrival {
        route,
        at: Utc::now(),
        added,
    });
    if arrivals.len() > MAX_ARRIVALS {
        arrivals.drain(..arrivals.len() - MAX_ARRIVALS);
    }
    if let Err(error) = write_json(&path, &arrivals) {
        eprintln!("받은 기록을 쓰지 못했다 ({}): {error}", document_dir.display());
    }
}

pub fn load_arrivals(document_dir: &Path) -> Vec<Arrival> {
    read_json_optional(&document_dir.join(ARRIVALS_FILE)).ok().flatten().unwrap_or_default()
}

/// 보관 대상인지(iPhone에서 온 문서)
pub fn should_archive(document_dir: &Path, manifest: &Manifest) -> bool {
    document_dir.join(ARRIVALS_FILE).is_file() || manifest.pages.iter().any(|p| p.raw.to_ascii_lowercase().ends_with(".heic"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ArchiveState {
    /// 지금 판의 원본과 manifest가 다 archive에 있다
    Done,
    /// 아직 덜 넣었다(다음 훑기에서 마저 한다)
    Partial,
    /// 보관 대상이 아니다(Mac에서 가져온 문서)
    NotApplicable,
}

/// 문서 하나를 archive에 맞춘다. 여러 번 불러도 같은 결과다.
/// `inbox_dir`을 주면 그 안에 있는 같은 원본을 옮겨 쓴다(iCloud로 받은 직후)
pub fn archive_document(store: &Store, root: &Path, id: Uuid, inbox_dir: Option<&Path>) -> Result<ArchiveState, String> {
    let library = store.document_dir(id);
    let manifest = Manifest::load(&library).map_err(|e| e.to_string())?;
    if !should_archive(&library, &manifest) {
        return Ok(ArchiveState::NotApplicable);
    }
    let target = root.join(id.to_string());
    for page in &manifest.pages {
        let destination = target.join(&page.raw);
        if is_present(&destination) {
            continue;
        }
        let parent = destination.parent().expect("raw/ 아래 경로라 부모가 있다");
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        // inbox의 원본은 iPhone이 이미 iCloud에 올린 것이다. 옮기면 다시 올리지 않는다.
        // 내용이 아직 Mac에 안 내려온(dataless) 파일도 옮길 수 있다. 옛 방식 자리표시(.이름.icloud)는 옮기지 않는다
        if let Some(inbox_dir) = inbox_dir {
            let source = inbox_dir.join(&page.raw);
            if source.is_file() && fs::rename(&source, &destination).is_ok() {
                continue;
            }
        }
        let source = library.join(&page.raw);
        let temp = parent.join(format!(".{}.archiving", Uuid::new_v4()));
        let copied = fs::copy(&source, &temp).and_then(|_| fs::rename(&temp, &destination));
        if let Err(error) = copied {
            let _ = fs::remove_file(&temp);
            return Err(format!("{} 보관 실패: {error}", page.raw));
        }
    }
    let manifest_bytes = fs::read(library.join(MANIFEST_FILE)).map_err(|e| e.to_string())?;
    let manifest_path = target.join(MANIFEST_FILE);
    if fs::read(&manifest_path).ok().as_deref() != Some(manifest_bytes.as_slice()) {
        write_atomic(&manifest_path, &manifest_bytes).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    }
    Ok(ArchiveState::Done)
}

/// 이 문서가 지금 archive에 다 있는지. 파일을 쓰지 않고 본다(화면 표시용)
pub fn archive_state(store: &Store, root: Option<&Path>, id: Uuid) -> ArchiveState {
    let library = store.document_dir(id);
    let Ok(manifest) = Manifest::load(&library) else {
        return ArchiveState::NotApplicable;
    };
    if !should_archive(&library, &manifest) {
        return ArchiveState::NotApplicable;
    }
    let Some(root) = root else {
        return ArchiveState::Partial;
    };
    let target = root.join(id.to_string());
    let complete = manifest.pages.iter().all(|p| is_present(&target.join(&p.raw)))
        && fs::read(target.join(MANIFEST_FILE)).ok() == fs::read(library.join(MANIFEST_FILE)).ok();
    if complete { ArchiveState::Done } else { ArchiveState::Partial }
}

/// 보관함 전체를 훑어 빠진 것을 채운다. 앱을 켤 때와 inbox 훑기 사이사이에 부른다.
/// 이 기능 전에 받은 문서와, Wi-Fi로 받은 뒤 복사가 실패한 문서를 여기서 마저 한다
pub fn backfill(store: &Store, root: &Path) -> (usize, usize) {
    let Ok(entries) = fs::read_dir(store.library_dir()) else {
        return (0, 0);
    };
    let (mut done, mut failed) = (0, 0);
    for entry in entries.flatten() {
        let Ok(id) = Uuid::parse_str(&entry.file_name().to_string_lossy()) else {
            continue;
        };
        if archive_state(store, Some(root), id) != ArchiveState::Partial {
            continue;
        }
        match archive_document(store, root, id, None) {
            Ok(_) => done += 1,
            Err(error) => {
                failed += 1;
                eprintln!("iCloud 보관 실패 ({id}): {error}");
            }
        }
    }
    (done, failed)
}

/// 문서 화면의 "받은 경로와 iCloud 보관" 줄
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentOrigin {
    /// 받은 기록. 오래된 것부터. 이 기능 전에 받은 문서는 비어 있다
    pub arrivals: Vec<Arrival>,
    pub archive: ArchiveState,
}

#[tauri::command]
pub fn document_origin(state: tauri::State<'_, crate::inbox::InboxState>, document_id: String) -> Result<DocumentOrigin, String> {
    let id = Uuid::parse_str(&document_id).map_err(|e| e.to_string())?;
    let store = state.store();
    let root = state.inbox_path().map(archive_root);
    Ok(DocumentOrigin {
        arrivals: load_arrivals(&store.document_dir(id)),
        archive: archive_state(store, root.as_deref(), id),
    })
}

/// 파일이 있거나, iCloud가 아직 내용을 안 내린 자리표시가 있다
fn is_present(path: &Path) -> bool {
    if path.is_file() {
        return true;
    }
    let Some(name) = path.file_name() else {
        return false;
    };
    path.with_file_name(format!(".{}.icloud", name.to_string_lossy())).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use scanshelf_core::{DocumentKind, Page, PageLayout, SCHEMA_VERSION};

    fn manifest(id: Uuid, raws: &[&str]) -> Manifest {
        Manifest {
            schema_version: SCHEMA_VERSION,
            id,
            name: "시험".into(),
            kind: DocumentKind::Pamphlet,
            session: None,
            created_at: Utc::now(),
            pages: raws
                .iter()
                .map(|raw| Page {
                    id: Uuid::new_v4(),
                    raw: raw.to_string(),
                    captured_at: Utc::now(),
                    layout: PageLayout::Single,
                    quad: None,
                    extra_rotation: 0,
                })
                .collect(),
            revision: Some(1),
        }
    }

    fn library_doc(store: &Store, manifest: &Manifest) {
        let dir = store.document_dir(manifest.id);
        fs::create_dir_all(dir.join("raw")).unwrap();
        for page in &manifest.pages {
            fs::write(dir.join(&page.raw), format!("photo {}", page.raw)).unwrap();
        }
        manifest.save(&dir).unwrap();
    }

    #[test]
    fn 보관함의_원본을_archive에_복사하고_manifest를_마지막에_쓴다() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(&temp.path().join("app"));
        let root = temp.path().join("archive");
        let id = Uuid::new_v4();
        let m = manifest(id, &["raw/a.heic", "raw/b.heic"]);
        library_doc(&store, &m);

        assert_eq!(archive_state(&store, Some(&root), id), ArchiveState::Partial);
        assert_eq!(archive_document(&store, &root, id, None).unwrap(), ArchiveState::Done);
        assert_eq!(fs::read_to_string(root.join(id.to_string()).join("raw/a.heic")).unwrap(), "photo raw/a.heic");
        assert!(root.join(id.to_string()).join(MANIFEST_FILE).is_file());
        assert_eq!(archive_state(&store, Some(&root), id), ArchiveState::Done);
    }

    #[test]
    fn inbox에_있는_원본은_복사하지_않고_옮긴다() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(&temp.path().join("app"));
        let root = temp.path().join("archive");
        let inbox = temp.path().join("inbox").join("doc");
        let id = Uuid::new_v4();
        let m = manifest(id, &["raw/a.heic"]);
        library_doc(&store, &m);
        fs::create_dir_all(inbox.join("raw")).unwrap();
        fs::write(inbox.join("raw/a.heic"), "from inbox").unwrap();

        archive_document(&store, &root, id, Some(&inbox)).unwrap();

        assert!(!inbox.join("raw/a.heic").exists());
        assert_eq!(fs::read_to_string(root.join(id.to_string()).join("raw/a.heic")).unwrap(), "from inbox");
    }

    #[test]
    fn 지운_장의_원본은_archive에_남는다() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(&temp.path().join("app"));
        let root = temp.path().join("archive");
        let id = Uuid::new_v4();
        library_doc(&store, &manifest(id, &["raw/a.heic", "raw/b.heic"]));
        archive_document(&store, &root, id, None).unwrap();

        let mut fewer = manifest(id, &["raw/a.heic"]);
        fewer.revision = Some(2);
        fewer.save(&store.document_dir(id)).unwrap();
        archive_document(&store, &root, id, None).unwrap();

        assert!(root.join(id.to_string()).join("raw/b.heic").is_file());
        assert_eq!(archive_state(&store, Some(&root), id), ArchiveState::Done);
    }

    #[test]
    fn mac에서_가져온_문서는_보관하지_않는다() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(&temp.path().join("app"));
        let root = temp.path().join("archive");
        let id = Uuid::new_v4();
        library_doc(&store, &manifest(id, &["raw/a.jpg"]));

        assert_eq!(archive_document(&store, &root, id, None).unwrap(), ArchiveState::NotApplicable);
        assert!(!root.join(id.to_string()).exists());
    }

    #[test]
    fn 받은_기록이_있으면_jpeg_문서도_보관한다() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(&temp.path().join("app"));
        let id = Uuid::new_v4();
        library_doc(&store, &manifest(id, &["raw/a.jpg"]));
        record_arrival(&store.document_dir(id), Route::Wifi, 1);

        assert_eq!(archive_state(&store, None, id), ArchiveState::Partial);
        assert_eq!(load_arrivals(&store.document_dir(id))[0].route, Route::Wifi);
    }

    #[test]
    fn 백필은_빠진_문서만_채운다() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(&temp.path().join("app"));
        let root = temp.path().join("archive");
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        library_doc(&store, &manifest(a, &["raw/a.heic"]));
        library_doc(&store, &manifest(b, &["raw/b.jpg"]));

        assert_eq!(backfill(&store, &root), (1, 0));
        assert_eq!(backfill(&store, &root), (0, 0));
    }
}
