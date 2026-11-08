//! iCloud 폴더 받기. iPhone은 찍을 때마다 `iCloud Drive/scanshelf/inbox/<문서 id>/`에 새 원본을, 그다음 manifest를
//! 복사한다. Mac은 manifest가 있고 원본이 모두 있는 문서를 보관함으로 가져온 뒤 `received/<문서 id>.json`에 받은
//! 것을 적고, 가져온 파일만 inbox에서 지운다. 폴더를 통째로 지우지 않는 것은 그사이 iPhone이 올린 다음 장을
//! 같이 지우지 않기 위해서다.
//!
//! 폴더 감시는 5초 간격 훑기로 한다. iCloud 동기화 자체가 수 초 걸리고, 파일 이벤트는 iCloud가 자리표시를
//! 실제 파일로 바꾸는 순간을 놓치기 쉬워 훑기가 더 단순하고 확실하다.
//!
//! 훑기와 가져오기(`scan_inbox`)는 플랫폼과 무관하게 두어 임시 폴더로 시험한다. iCloud 경로와 내려받기
//! 요청만 macOS 전용이다.
//!
//! @see docs/transfer.md

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use scanshelf_core::fsio::write_atomic;
use scanshelf_core::{MANIFEST_FILE, Manifest};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::store::{ImportMode, RAW_DIR, Store, StoreError, is_raw_path, list_raw_files};

/// 확인 파일 폴더 이름(`scanshelf/received`)
const RECEIVED_DIR: &str = "received";
/// 한 달이 지난 확인 파일은 앱을 켤 때 지운다
#[cfg(target_os = "macos")]
const RECEIPT_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct InboxScan {
    pub imported: Vec<Uuid>,
    /// 아직 가져오지 못한 문서 폴더 수(동기화 중, 내려받는 중, 오류)
    pub pending: u32,
    /// 아직 가져오지 못한 문서마다 이름과 이유. 설정 화면이 개수만이 아니라 무엇을 기다리는지 보여 준다
    pub waiting: Vec<PendingDocument>,
    /// 가져오기에 실패한 문서와 이유. 감시 고리가 같은 오류를 5초마다 되풀이해 남기지 않도록 부른 쪽이 거른다
    pub failed: Vec<(Uuid, String)>,
}

/// 파일이 실제로 받아져 있는지. iCloud는 아직 받지 않은 파일을 두 가지로 보인다:
/// 옛 방식은 `.이름.icloud` 자리표시 파일, macOS 14 이후 FileProvider 방식은 크기만 있고 내용이 없는(dataless) 파일
#[derive(Debug, PartialEq, Eq)]
enum FileState {
    Ready,
    /// 자리표시만 있다. 내려받기를 요청해야 한다
    NotDownloaded,
    /// 흔적이 없다. 아직 iCloud가 목록을 동기화하지 않았다
    Absent,
}

fn file_state(path: &Path) -> FileState {
    if let Ok(metadata) = fs::metadata(path)
        && metadata.is_file()
    {
        return if is_dataless(&metadata) {
            FileState::NotDownloaded
        } else {
            FileState::Ready
        };
    }
    if placeholder_path(path).is_some_and(|p| p.exists()) {
        FileState::NotDownloaded
    } else {
        FileState::Absent
    }
}

fn placeholder_path(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy();
    Some(path.with_file_name(format!(".{name}.icloud")))
}

#[cfg(target_os = "macos")]
fn is_dataless(metadata: &fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    // <sys/stat.h> SF_DATALESS: 내용이 아직 내려오지 않은 FileProvider 파일
    const SF_DATALESS: u32 = 0x4000_0000;
    metadata.st_flags() & SF_DATALESS != 0
}

#[cfg(not(target_os = "macos"))]
fn is_dataless(_metadata: &fs::Metadata) -> bool {
    false
}

/// Mac이 무엇을 받았는지 iPhone에 알리는 파일(`scanshelf/received/<문서 id>.json`).
/// iCloud에서 폴더가 사라지는 것만으로는 어느 장까지 가져갔는지 알 수 없어 이 파일로 확인한다
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub document_id: Uuid,
    /// 가져온 manifest 바이트의 SHA-256. iPhone의 manifest 지문과 같으면 그 판이 Mac에 있다는 뜻이다
    pub manifest_sha256: Option<String>,
    /// 보관함에 있는 원본 상대 경로 전부. 비어 있으면 Mac에서 문서를 지웠으니 원본을 처음부터 다시 올리라는 뜻이다
    pub raw: Vec<String>,
}

/// 확인 파일을 두는 폴더. inbox 옆이다
pub fn received_dir(inbox: &Path) -> PathBuf {
    inbox.with_file_name(RECEIVED_DIR)
}

fn receipt_path(inbox: &Path, id: Uuid) -> PathBuf {
    received_dir(inbox).join(format!("{id}.json"))
}

fn write_receipt(inbox: &Path, receipt: &Receipt) -> Result<(), String> {
    let path = receipt_path(inbox, receipt.document_id);
    let text = serde_json::to_vec_pretty(receipt).map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    write_atomic(&path, &text).map_err(|e| format!("{}: {e}", path.display()))
}

/// 오래된 확인 파일을 지운다. iPhone이 그 뒤에 와서 못 읽어도 원본을 다시 올리면 같은 파일이 다시 생겨 손해가 없다
pub fn prune_receipts(inbox: &Path, max_age: Duration) {
    let Ok(entries) = fs::read_dir(received_dir(inbox)) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= max_age);
        if old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// inbox를 한 번 훑는다. 준비된 문서는 보관함으로 복사한 뒤 가져온 파일을 inbox에서 지우고, 받지 않은 파일은
/// `request_download`로 요청한다
pub fn scan_inbox(inbox: &Path, store: &Store, request_download: &mut dyn FnMut(&Path)) -> InboxScan {
    let mut scan = InboxScan::default();
    let Ok(entries) = fs::read_dir(inbox) else {
        return scan;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !dir.is_dir() {
            continue;
        }
        let Ok(id) = Uuid::parse_str(&name) else {
            continue;
        };
        match import_one(&dir, id, inbox, store, request_download) {
            Ok(Step::Imported) => scan.imported.push(id),
            Ok(Step::ImportedWithoutReceipt(error)) => {
                // 가져오기는 끝났지만 iPhone에 알리지 못했다. inbox를 그대로 두고 다음 훑기에서 다시 적는다
                scan.imported.push(id);
                scan.waiting.push(PendingDocument {
                    document_id: id,
                    name: None,
                    reason: "받았지만 iPhone에 알리지 못함".into(),
                });
                scan.failed.push((id, error));
                scan.pending += 1;
            }
            Ok(Step::Waiting { name, reason }) => {
                scan.waiting.push(PendingDocument {
                    document_id: id,
                    name,
                    reason,
                });
                scan.pending += 1;
            }
            Ok(Step::Empty) => {}
            Err(error) => {
                scan.waiting.push(PendingDocument {
                    document_id: id,
                    name: None,
                    reason: format!("오류: {error}"),
                });
                scan.failed.push((id, error));
                scan.pending += 1;
            }
        }
    }
    scan
}

/// inbox에서 아직 가져오지 못한 문서
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingDocument {
    pub document_id: Uuid,
    /// manifest를 아직 못 읽었으면 없다
    pub name: Option<String>,
    pub reason: String,
}

enum Step {
    Imported,
    ImportedWithoutReceipt(String),
    /// 아직 준비가 안 됐다
    Waiting { name: Option<String>, reason: String },
    /// 비어 있던 폴더를 치웠다
    Empty,
}

fn import_one(
    dir: &Path,
    id: Uuid,
    inbox: &Path,
    store: &Store,
    request_download: &mut dyn FnMut(&Path),
) -> Result<Step, String> {
    let manifest_path = dir.join(MANIFEST_FILE);
    match file_state(&manifest_path) {
        FileState::Ready => {}
        FileState::NotDownloaded => {
            request_download(&manifest_path);
            return Ok(Step::Waiting {
                name: None,
                reason: "문서 정보를 iCloud에서 내려받는 중".into(),
            });
        }
        FileState::Absent => {
            return Ok(if remove_if_empty(dir) {
                Step::Empty
            } else {
                Step::Waiting {
                    name: None,
                    reason: "iPhone이 올리는 중".into(),
                }
            });
        }
    }
    // 한 번 읽은 바이트로 해시와 내용을 함께 얻는다. 따로 읽으면 그 사이 iPhone이 바꾼 판을 받았다고 잘못 알린다
    let bytes = fs::read(&manifest_path).map_err(|e| format!("manifest를 읽지 못했다: {e}"))?;
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|e| format!("manifest 형식 오류: {e}"))?;
    manifest.validate().map_err(|e| e.to_string())?;
    if manifest.id != id {
        return Err(format!("폴더 이름과 manifest id({})가 다르다", manifest.id));
    }

    // 옛 판이면 원본이 빠져 있어도(iPhone에서 지운 장) 기다리지 않는다. 가져오기가 아무것도 바꾸지 않고 끝낸다
    if !store.is_stale(&manifest) {
        let library = store.document_dir(id);
        let mut downloading = 0usize;
        let mut absent_count = 0usize;
        for page in &manifest.pages {
            if !is_raw_path(&page.raw) {
                return Err(format!("raw 경로는 raw/ 아래여야 한다: {}", page.raw));
            }
            // 보관함에 이미 있으면 inbox 쪽은 보지 않는다. 앞서 가져간 장은 iPhone이 다시 올리지 않고,
            // 자리표시로 남아 있어도 내려받을 필요가 없다
            if library.join(&page.raw).is_file() {
                continue;
            }
            let path = dir.join(&page.raw);
            match file_state(&path) {
                FileState::Ready => {}
                FileState::NotDownloaded => {
                    request_download(&path);
                    downloading += 1;
                }
                FileState::Absent => absent_count += 1,
            }
        }
        if downloading + absent_count > 0 {
            if absent_count > 0 && !library.join(MANIFEST_FILE).is_file() {
                ask_for_everything_again(inbox, id);
            }
            let reason = if absent_count > 0 {
                format!("원본 {absent_count}장을 iPhone이 올리는 중")
            } else {
                format!("원본 {downloading}장을 iCloud에서 내려받는 중")
            };
            return Ok(Step::Waiting {
                name: Some(manifest.name.clone()),
                reason,
            });
        }
    }

    let _busy = crate::busy::start(format!("iCloud에서 가져오는 중: {}", manifest.name));
    let outcome = match store.import(&manifest, dir, ImportMode::Copy) {
        Ok(outcome) => outcome,
        Err(StoreError::MissingFiles(missing)) => {
            return Ok(Step::Waiting {
                name: Some(manifest.name.clone()),
                reason: format!("원본 {}장을 기다리는 중", missing.len()),
            });
        }
        Err(error) => return Err(error.to_string()),
    };
    if outcome.stale {
        eprintln!("iCloud inbox의 옛 manifest를 건너뜀: {id}");
    } else {
        eprintln!("문서 받음 (iCloud): {id} (새 원본 {}개)", outcome.added_raw.len());
        crate::archive::record_arrival(&store.document_dir(id), crate::archive::Route::Icloud, outcome.added_raw.len());
    }

    // 확인 파일을 먼저 쓰고 지운다. 순서가 반대면 멈췄을 때 iPhone이 알 길 없이 파일만 사라진다
    let raw = list_raw_files(&store.document_dir(id)).map_err(|e| e.to_string())?;
    let receipt = Receipt {
        document_id: id,
        manifest_sha256: Some(hex::encode(Sha256::digest(&bytes))),
        raw,
    };
    if let Err(error) = write_receipt(inbox, &receipt) {
        return Ok(Step::ImportedWithoutReceipt(format!("받은 것을 iPhone에 알리지 못했다: {error}")));
    }
    // 지우기 전에 원본을 archive로 옮긴다(영구 보관). 같은 iCloud Drive 안의 이름 바꾸기라 다시 올리지 않는다.
    // 실패하면 inbox 쪽은 아래에서 지우고, 백필이 보관함에서 복사해 채운다
    if !outcome.stale
        && let Err(error) = crate::archive::archive_document(store, &crate::archive::archive_root(inbox), id, Some(dir))
    {
        eprintln!("iCloud 보관 실패 ({id}), 다음 백필에서 다시 한다: {error}");
    }
    clear_imported(dir, &manifest, &bytes);
    Ok(Step::Imported)
}

/// iPhone이 "이미 Mac에 있다"고 믿고 올리지 않은 원본을 Mac에서 지웠을 때(보관함에서 문서 삭제) 영영 기다리지 않게,
/// 앞서 알린 확인 파일이 있으면 비운다. iPhone은 빈 확인 파일을 보면 원본을 처음부터 다시 올린다
fn ask_for_everything_again(inbox: &Path, id: Uuid) {
    let path = receipt_path(inbox, id);
    let told = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Receipt>(&bytes).ok())
        .is_some_and(|receipt| !receipt.raw.is_empty());
    if !told {
        return;
    }
    let reset = Receipt {
        document_id: id,
        manifest_sha256: None,
        raw: Vec::new(),
    };
    if let Err(error) = write_receipt(inbox, &reset) {
        eprintln!("확인 파일을 비우지 못했다 ({id}): {error}");
    }
}

/// 이번에 가져온 파일만 지운다. manifest는 읽은 뒤 바뀌지 않았을 때만 지운다. 바뀌었으면 다음 판이 이미 도착한 것이다.
/// 지우지 못해도 보관함에는 들어갔으니 다음 훑기에서 다시 해도 같은 결과다
fn clear_imported(dir: &Path, manifest: &Manifest, manifest_bytes: &[u8]) {
    for page in &manifest.pages {
        let path = dir.join(&page.raw);
        let _ = fs::remove_file(&path);
        if let Some(placeholder) = placeholder_path(&path) {
            let _ = fs::remove_file(placeholder);
        }
    }
    let manifest_path = dir.join(MANIFEST_FILE);
    if fs::read(&manifest_path).is_ok_and(|now| now == manifest_bytes) {
        let _ = fs::remove_file(&manifest_path);
    }
    remove_if_empty(dir);
}

/// 비었으면 문서 폴더를 치운다. 남은 파일이 있으면(다음 장, 지운 장의 원본) 그대로 둔다
fn remove_if_empty(dir: &Path) -> bool {
    for folder in [dir.join(RAW_DIR), dir.to_path_buf()] {
        let _ = fs::remove_file(folder.join(".DS_Store"));
        let _ = fs::remove_dir(&folder);
    }
    !dir.exists()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxStatus {
    pub path: String,
    pub available: bool,
    pub pending: u32,
    /// 아직 가져오지 못한 문서와 이유
    pub waiting: Vec<PendingDocument>,
    /// iCloud archive 폴더
    pub archive_path: String,
    /// archive에 다 들어간 문서 수와 아직 덜 들어간 문서 수(보관 대상만)
    pub archived: u32,
    pub archive_pending: u32,
}

pub struct InboxState {
    path: Option<PathBuf>,
    pending: Arc<AtomicU32>,
    waiting: Arc<std::sync::Mutex<Vec<PendingDocument>>>,
    store: Arc<Store>,
}

impl InboxState {
    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn inbox_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

#[tauri::command]
pub fn icloud_inbox_status(state: tauri::State<'_, InboxState>) -> InboxStatus {
    let archive = state.path.as_deref().map(crate::archive::archive_root);
    let (mut archived, mut archive_pending) = (0, 0);
    if let Ok(entries) = fs::read_dir(state.store.library_dir()) {
        for entry in entries.flatten() {
            let Ok(id) = Uuid::parse_str(&entry.file_name().to_string_lossy()) else {
                continue;
            };
            match crate::archive::archive_state(&state.store, archive.as_deref(), id) {
                crate::archive::ArchiveState::Done => archived += 1,
                crate::archive::ArchiveState::Partial => archive_pending += 1,
                crate::archive::ArchiveState::NotApplicable => {}
            }
        }
    }
    InboxStatus {
        path: state
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        available: state.path.as_ref().is_some_and(|p| p.is_dir()),
        pending: state.pending.load(Ordering::SeqCst),
        waiting: state.waiting.lock().map(|w| w.clone()).unwrap_or_default(),
        archive_path: archive.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        archived,
        archive_pending,
    }
}

/// `~/Library/Mobile Documents/com~apple~CloudDocs/scanshelf/inbox`. iCloud Drive가 꺼져 있으면 None
#[cfg(target_os = "macos")]
pub fn icloud_inbox_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let drive = PathBuf::from(home).join("Library/Mobile Documents/com~apple~CloudDocs");
    drive.is_dir().then(|| drive.join("scanshelf").join("inbox"))
}

#[cfg(not(target_os = "macos"))]
pub fn icloud_inbox_dir() -> Option<PathBuf> {
    None
}

/// 이름을 바꾸기 전 iPhone 앱이 쓰던 `iCloud Drive/leaf/inbox`. 이미 올려 둔 문서를 잃지 않도록 있으면 같이 훑는다.
/// 새로 만들지는 않는다
#[cfg(target_os = "macos")]
fn legacy_inbox_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let dir = PathBuf::from(home).join("Library/Mobile Documents/com~apple~CloudDocs/leaf/inbox");
    dir.is_dir().then_some(dir)
}

/// 같은 파일에 내려받기를 너무 자주 요청하지 않는다. 큰 사진은 받는 데 수십 초가 걸린다
#[cfg(target_os = "macos")]
fn download_requester() -> impl FnMut(&Path) {
    use std::collections::HashMap;
    use std::time::Instant;

    let mut requested: HashMap<PathBuf, Instant> = HashMap::new();
    move |path: &Path| {
        let now = Instant::now();
        requested.retain(|_, at| now.duration_since(*at) < Duration::from_secs(60));
        if requested.contains_key(path) {
            return;
        }
        requested.insert(path.to_path_buf(), now);
        // brctl은 요청만 넣고 바로 끝난다. 실제 내려받기는 iCloud 데몬이 한다
        match std::process::Command::new("brctl").arg("download").arg(path).spawn() {
            Ok(mut child) => {
                std::thread::spawn(move || child.wait());
            }
            Err(error) => eprintln!("brctl download 실패: {error}"),
        }
    }
}

/// iCloud inbox 감시를 시작한다. iCloud Drive가 없거나 macOS가 아니면 상태만 등록한다
pub fn setup(app: &tauri::AppHandle, store: Arc<Store>) {
    use tauri::Manager;

    let path = icloud_inbox_dir();
    let pending = Arc::new(AtomicU32::new(0));
    let waiting = Arc::new(std::sync::Mutex::new(Vec::new()));
    app.manage(InboxState {
        path: path.clone(),
        pending: pending.clone(),
        waiting: waiting.clone(),
        store: store.clone(),
    });

    #[cfg(target_os = "macos")]
    if let Some(inbox) = path {
        // iPhone 파일 앱에서 고를 폴더가 보이도록 미리 만든다
        if let Err(error) = fs::create_dir_all(&inbox) {
            eprintln!("iCloud inbox 폴더를 만들지 못했다: {error}");
        }
        use std::collections::HashSet;

        let app = app.clone();
        std::thread::spawn(move || {
            prune_receipts(&inbox, RECEIPT_MAX_AGE);
            let archive = crate::archive::archive_root(&inbox);
            let mut request = download_requester();
            let mut reported: HashSet<(Uuid, String)> = HashSet::new();
            let mut round: u64 = 0;
            loop {
                let mut count = 0;
                let mut names = Vec::new();
                for dir in std::iter::once(inbox.clone()).chain(legacy_inbox_dir()) {
                    let scan = scan_inbox(&dir, &store, &mut request);
                    count += scan.pending;
                    names.extend(scan.waiting);
                    for id in scan.imported {
                        crate::transfer::emit_library_changed(&app, id);
                    }
                    for failure in scan.failed {
                        if !reported.contains(&failure) {
                            eprintln!("iCloud inbox 문서를 가져오지 못했다 ({}): {}", failure.0, failure.1);
                            reported.insert(failure);
                        }
                    }
                }
                pending.store(count, Ordering::SeqCst);
                if let Ok(mut slot) = waiting.lock() {
                    *slot = names;
                }
                // Wi-Fi로 받은 문서와 이 기능 전에 받은 문서를 1분마다 archive에 채운다. 첫 훑기에서 바로 한 번 한다
                if round.is_multiple_of(12) {
                    let (done, failed) = crate::archive::backfill(&store, &archive);
                    if done + failed > 0 {
                        eprintln!("iCloud 보관: {done}개 채움, {failed}개 실패");
                    }
                }
                round += 1;
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (store, pending, waiting);
}
