import Foundation

// iCloud 백업이 어디까지 됐는지와 다음에 할 일. 파일을 만지지 않는 판단만 둔다.
// 할 일 목록을 따로 저장하지 않고 문서와 이 기록에서 매번 계산한다. 앱이 어느 순간에 죽어도 밀린 일이 사라지지 않는다.
// @see docs/transfer.md, docs/ios.md

struct BackupEntry: Codable, Equatable, Sendable {
    /// manifest까지 iCloud 폴더에 복사를 끝낸 지문
    var copiedFingerprint: String?
    /// iCloud가 올라갔다고 알려 준 지문. 화면의 "iCloud에 있음"은 이 값으로 정한다
    var uploadedFingerprint: String?
    var copiedAt: Date?
    /// Mac에 있다고 확인된 원본 상대 경로. 이 원본은 다시 올리지 않는다
    var macRaws: Set<String> = []
    /// 연달아 실패한 횟수
    var attempts = 0
    /// 같은 판을 다시 올린 횟수
    var recopies = 0
    var lastError: String?
    /// Mac이 iCloud archive에 원본을 다 넣었다고 iPhone에서 보인 지문. 화면의 "iCloud 보관"과 원본 자동 삭제가 이 값을 본다.
    /// 옛 기록에는 없어 Optional이다
    var archivedFingerprint: String?
    /// archive를 확인한 횟수. 간격을 늘리는 데 쓴다
    var archiveChecks: Int?
}

struct BackupState: Codable, Equatable, Sendable {
    var entries: [UUID: BackupEntry] = [:]
    /// 이 iPhone에서 지웠고 iCloud 사본을 아직 못 치운 문서. Mac이 뒤늦게 가져가지 않게 앱을 다시 켜도 이어서 치운다
    var deletedDocuments: Set<UUID> = []
}

/// Application Support/backup-state.json. 문서 폴더 밖에 두어 iCloud나 Mac으로 섞여 가지 않는다.
/// 읽지 못하면 빈 기록으로 시작한다. 이미 있는 파일은 크기로 건너뛰고 Mac이 다시 확인해 주므로 잃는 것이 없다
struct BackupStateStore: Sendable {
    let url: URL

    static func appDefault() -> BackupStateStore {
        BackupStateStore(url: URL.applicationSupportDirectory.appending(path: "backup-state.json"))
    }

    func load() -> BackupState {
        guard let data = try? Data(contentsOf: url),
              let state = try? JSONDecoder.scanshelf.decode(BackupState.self, from: data)
        else { return BackupState() }
        return state
    }

    func save(_ state: BackupState) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try DocumentStore.writeAtomically(try JSONEncoder.scanshelf.encode(state), to: url)
    }
}

enum BackupWork: Equatable, Sendable {
    case none
    /// 지금 판을 iCloud 폴더에 올려야 한다
    case copy
    /// 올렸으니 Mac의 확인 파일을 기다린다
    case watch
    /// Mac이 받았으니 iCloud 사본을 치운다
    case cleanup
    /// Mac이 받았고 inbox도 비웠다. Mac이 archive에 원본을 다 넣었는지 본다
    case verifyArchive
}

struct BackupItem: Equatable, Sendable {
    var id: UUID
    var work: BackupWork
}

enum BackupQueue {
    /// Mac의 확인 파일을 보는 간격
    static let watchInterval: TimeInterval = 20
    /// 폴더에 닿지 않을 때 다시 해 보는 간격
    static let folderRetryDelay: TimeInterval = 60
    private static let maxDelay: TimeInterval = 3600

    static func work(hasPages: Bool, fingerprint: String, record: SendRecord?, entry: BackupEntry?) -> BackupWork {
        if record?.confirmed == true, record?.manifestFingerprint == fingerprint {
            if entry?.copiedFingerprint != nil { return .cleanup }
            guard hasPages else { return .none }
            return entry?.archivedFingerprint == fingerprint ? .none : .verifyArchive
        }
        guard let entry else { return hasPages ? .copy : .none }
        if !hasPages, entry.macRaws.isEmpty, entry.copiedFingerprint == nil { return .none }
        return entry.copiedFingerprint == fingerprint ? .watch : .copy
    }

    /// 문서마다 할 일. 지문 계산이 페이지 수만큼 걸리니 메인 스레드 밖에서 부른다
    static func plan(documents: [Manifest], records: [UUID: SendRecord], entries: [UUID: BackupEntry]) -> [BackupItem] {
        documents.compactMap { manifest in
            guard let fingerprint = try? manifest.fingerprint() else { return nil }
            let work = work(hasPages: !manifest.pages.isEmpty, fingerprint: fingerprint, record: records[manifest.id], entry: entries[manifest.id])
            return work == .none ? nil : BackupItem(id: manifest.id, work: work)
        }
    }

    /// archive 확인 간격. Mac은 1분마다 채우고 iCloud가 그걸 내려 주는 데 또 걸린다. 20초에서 두 배씩, 1시간까지
    static func archiveCheckDelay(checks: Int) -> TimeInterval {
        min(watchInterval * pow(2, Double(max(checks, 0))), maxDelay)
    }

    /// 실패 뒤 기다리는 시간. 15초에서 시작해 두 배씩, 1시간까지
    static func retryDelay(attempts: Int) -> TimeInterval {
        min(15 * pow(2, Double(max(attempts, 1) - 1)), maxDelay)
    }

    /// 올린 파일이 폴더에서 사라졌는데 확인 파일이 없을 때 다시 올리기 전에 기다리는 시간.
    /// 확인 파일이 삭제보다 늦게 내려올 수 있어 바로 올리지 않는다. Mac이 옛 버전이면 확인 파일이 영영 없으니 간격을 늘린다
    static func recopyGrace(recopies: Int) -> TimeInterval {
        min(120 * pow(2, Double(max(recopies, 0))), maxDelay)
    }
}

/// 스캔이 이 iPhone에만 있다는 안내
enum BackupNotice: Equatable, Sendable {
    case folderNotChosen
    case folderUnavailable(String)

    static func of(folderChosen: Bool, folderFailure: String?) -> BackupNotice? {
        guard folderChosen else { return .folderNotChosen }
        return folderFailure.map(BackupNotice.folderUnavailable)
    }

    var title: String {
        switch self {
        case .folderNotChosen: "스캔이 이 iPhone에만 있습니다"
        case .folderUnavailable: "iCloud 폴더에 닿지 않습니다"
        }
    }

    var message: String {
        switch self {
        case .folderNotChosen: "앱을 지우면 함께 사라집니다. iCloud 폴더를 고르면 찍을 때마다 복사합니다."
        case let .folderUnavailable(reason): "복사가 멈췄습니다. \(reason)"
        }
    }
}
