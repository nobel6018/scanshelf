import CryptoKit
import Foundation
import Testing
@testable import Scanshelf

// iCloud 백업의 대기열과 상태 판단, 파일 흐름. iCloud 대신 임시 폴더를 쓰고 합성 데이터만 쓴다.
// Mac 쪽 가져오기는 src-tauri/tests/inbox.rs가 확인한다. 여기의 FakeMac은 iPhone이 보는 결과만 흉내 낸다.

@MainActor
private final class FakeHost: BackupHost {
    var documents: [Manifest] = []
    var records: [UUID: SendRecord] = [:]
    var confirmed: [(UUID, String)] = []

    func backupConfirmed(_ documentID: UUID, fingerprint: String) {
        confirmed.append((documentID, fingerprint))
        records[documentID] = SendRecord(method: .icloud, sentAt: .now, manifestFingerprint: fingerprint, confirmed: true)
    }
}

private struct FakeMac {
    let root: URL
    let library: URL

    /// inbox를 가져오고 확인 파일을 쓴 뒤 가져온 파일만 지운다. 보관함 원본 목록은 파일로 쌓는다
    func importInbox(_ documentID: UUID) throws {
        let inbox = ICloudFolderLayout.inbox(in: root, documentID: documentID)
        let data = try Data(contentsOf: inbox.appending(path: "manifest.json"))
        let manifest = try Manifest.decoder().decode(Manifest.self, from: data)
        let held = library.appending(path: documentID.uuidString.lowercased())
        try FileManager.default.createDirectory(at: held.appending(path: "raw"), withIntermediateDirectories: true)
        for page in manifest.pages {
            let source = inbox.appending(path: page.raw)
            let copy = held.appending(path: page.raw)
            if FileManager.default.fileExists(atPath: source.path), !FileManager.default.fileExists(atPath: copy.path) {
                try FileManager.default.copyItem(at: source, to: copy)
            }
            #expect(FileManager.default.fileExists(atPath: copy.path), "Mac이 받지 못한 원본: \(page.raw)")
            try? FileManager.default.removeItem(at: source)
        }
        try FileManager.default.removeItem(at: inbox.appending(path: "manifest.json"))
        let raws = try FileManager.default.contentsOfDirectory(atPath: held.appending(path: "raw").path).map { "raw/\($0)" }
        try writeReceipt(documentID, sha256: SHA256.hash(data: data).hexString, raw: raws)
        try? FileManager.default.removeItem(at: inbox.appending(path: "raw"))
        try? FileManager.default.removeItem(at: inbox)
    }

    /// Mac의 archive.rs가 하는 일: 보관함 원본과 manifest를 archive에 둔다
    func archive(_ manifest: Manifest) throws {
        let target = ICloudFolderLayout.archive(in: root, documentID: manifest.id)
        try FileManager.default.createDirectory(at: target.appending(path: "raw"), withIntermediateDirectories: true)
        for page in manifest.pages {
            try Data("archived".utf8).write(to: target.appending(path: page.raw))
        }
        try Data("{}".utf8).write(to: target.appending(path: "manifest.json"))
    }

    func writeReceipt(_ documentID: UUID, sha256: String?, raw: [String]) throws {
        let url = ICloudFolderLayout.receipt(in: root, documentID: documentID)
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        let sha = sha256.map { "\"\($0)\"" } ?? "null"
        let rawList = raw.map { "\"\($0)\"" }.joined(separator: ",")
        let text = "{\"documentId\":\"\(documentID.uuidString.lowercased())\",\"manifestSha256\":\(sha),\"raw\":[\(rawList)]}"
        try Data(text.utf8).write(to: url)
    }
}

@MainActor
private final class Rig {
    let base = FileManager.default.temporaryDirectory.appending(path: "scanshelf-backup-\(UUID().uuidString)")
    let store: DocumentStore
    let stateStore: BackupStateStore
    let root: URL
    let host = FakeHost()
    var mac: FakeMac
    var chosen = true
    var rootFailure: Error?
    var backup: ICloudBackup!
    private let automatic: Bool

    init(automatic: Bool = false) {
        self.automatic = automatic
        store = DocumentStore(root: base.appending(path: "library"))
        stateStore = BackupStateStore(url: base.appending(path: "support/backup-state.json"))
        root = base.appending(path: "com~apple~CloudDocs/scanshelf")
        mac = FakeMac(root: root, library: base.appending(path: "mac"))
        relaunch()
    }

    deinit {
        try? FileManager.default.removeItem(at: base)
    }

    /// 앱을 다시 켠 모습. 메모리 상태 없이 저장된 기록과 문서만 남는다
    func relaunch() {
        backup = ICloudBackup(store: store, stateStore: stateStore, automatic: automatic) { [weak self] in
            guard let self else { return nil }
            if let rootFailure { throw rootFailure }
            return chosen ? root : nil
        }
        backup.host = host
    }

    func newDocument() throws -> Manifest {
        let manifest = try store.create(name: "예시_1", kind: .document, session: nil)
        host.documents = [manifest]
        return manifest
    }

    func capture(_ manifest: Manifest, byte: UInt8, replacing: UUID? = nil) throws -> Manifest {
        let updated = try store.addPage(to: manifest, photo: Data([byte, byte]), quad: nil, layout: .single, capturedAt: .now, replacing: replacing)
        host.documents = [updated]
        return updated
    }

    func inbox(_ manifest: Manifest) -> URL {
        ICloudFolderLayout.inbox(in: root, documentID: manifest.id)
    }

    func inboxRaws(_ manifest: Manifest) -> Set<String> {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: inbox(manifest).appending(path: "raw").path)) ?? []
        return Set(names.map { "raw/\($0)" })
    }

    func inboxManifest(_ manifest: Manifest) throws -> Manifest {
        try Manifest.decoder().decode(Manifest.self, from: Data(contentsOf: inbox(manifest).appending(path: "manifest.json")))
    }

    func status(_ manifest: Manifest) -> SendStatus {
        SendStatus.resolve(
            record: host.records[manifest.id],
            fingerprint: try? manifest.fingerprint(),
            hasPages: !manifest.pages.isEmpty,
            folderChosen: chosen,
            backup: backup.state.entries[manifest.id],
            transferFailure: nil
        )
    }
}

/// 다음 판을 볼 시각까지 건너뛴다
private func later(_ seconds: TimeInterval) -> Date {
    Date.now.addingTimeInterval(seconds)
}

@MainActor
struct ICloudBackupFlowTests {
    @Test func 찍은_장을_iCloud에_복사하고_iCloud에_있음이_된다() async throws {
        let rig = Rig()
        let manifest = try rig.capture(try rig.newDocument(), byte: 1)
        #expect(rig.status(manifest) == .backupPending)

        await rig.backup.runDue()

        #expect(try rig.inboxManifest(manifest) == manifest)
        #expect(try Data(contentsOf: rig.inbox(manifest).appending(path: manifest.pages[0].raw)) == Data([1, 1]))
        #expect(rig.status(manifest) == .awaitingMac(.icloud))
        #expect(rig.host.confirmed.isEmpty)
    }

    @Test func 깨우기는_기다리지_않고_돌아오고_복사는_뒤에서_끝난다() async throws {
        let rig = Rig(automatic: true)
        let manifest = try rig.capture(try rig.newDocument(), byte: 1)

        rig.backup.kick(manifest.id)
        // kick이 복사를 끝내고 돌아왔다면 이미 파일이 있다. 메인 액터를 내주기 전에는 복사가 시작되지 않는다
        #expect(!FileManager.default.fileExists(atPath: rig.inbox(manifest).path))

        for _ in 0 ..< 200 where rig.backup.state.entries[manifest.id]?.copiedFingerprint == nil {
            try await Task.sleep(for: .milliseconds(20))
        }
        #expect(rig.status(manifest) == .awaitingMac(.icloud))
    }

    @Test func 폴더가_사라진_것만으로는_Mac이_받았다고_보지_않고_원본을_지우지_않는다() async throws {
        let rig = Rig()
        let manifest = try rig.capture(try rig.newDocument(), byte: 1)
        await rig.backup.runDue()

        // Mac이 지우는 사이 다음 장이 같이 사라진 경우와 구별할 수 없다
        try FileManager.default.removeItem(at: rig.inbox(manifest))
        await rig.backup.runDue(now: later(30))

        #expect(rig.host.confirmed.isEmpty)
        #expect(rig.host.records[manifest.id] == nil)
        let fingerprints = [manifest.id: try manifest.fingerprint()]
        #expect(RetentionPolicy.documentsToDelete(records: rig.host.records, fingerprints: fingerprints, days: 1, now: later(30 * 86400)).isEmpty)
        #expect(FileManager.default.fileExists(atPath: rig.store.fileURL(of: manifest.id, relativePath: manifest.pages[0].raw).path))
    }

    @Test func 폴더가_사라지고_확인이_없으면_시간을_두고_다시_올린다() async throws {
        let rig = Rig()
        let manifest = try rig.capture(try rig.newDocument(), byte: 1)
        await rig.backup.runDue()
        try FileManager.default.removeItem(at: rig.inbox(manifest))

        await rig.backup.runDue(now: later(30))
        #expect(!FileManager.default.fileExists(atPath: rig.inbox(manifest).path))

        await rig.backup.runDue(now: later(150))
        #expect(rig.inboxRaws(manifest) == [manifest.pages[0].raw])
        #expect(rig.backup.state.entries[manifest.id]?.recopies == 1)
    }

    @Test func 확인_파일의_지문이_지금_판이면_Mac에_있음이_되고_원본_삭제_대상이_된다() async throws {
        let rig = Rig()
        let manifest = try rig.capture(try rig.newDocument(), byte: 1)
        await rig.backup.runDue()

        try rig.mac.importInbox(manifest.id)
        await rig.backup.runDue(now: later(30))

        #expect(rig.host.confirmed.map(\.0) == [manifest.id])
        guard case .sent = rig.status(manifest) else {
            Issue.record("Mac에 있음이어야 한다: \(rig.status(manifest))")
            return
        }
        let fingerprints = [manifest.id: try manifest.fingerprint()]
        let expired = RetentionPolicy.documentsToDelete(records: rig.host.records, fingerprints: fingerprints, days: 7, now: later(8 * 86400))
        #expect(expired == [manifest.id])
    }

    @Test func Mac이_archive에_넣으면_iCloud_보관이_되고_그때부터_원본_삭제_대상이_된다() async throws {
        let rig = Rig()
        let manifest = try rig.capture(try rig.newDocument(), byte: 1)
        await rig.backup.runDue()
        try rig.mac.importInbox(manifest.id)
        await rig.backup.runDue(now: later(30))

        let fingerprint = try manifest.fingerprint()
        func cloud() -> CloudStatus {
            CloudStatus.resolve(folderChosen: true, hasPages: true, fingerprint: fingerprint, record: rig.host.records[manifest.id], backup: rig.backup.state.entries[manifest.id])
        }
        func expired() -> [UUID] {
            RetentionPolicy.documentsToDelete(
                records: rig.host.records,
                fingerprints: [manifest.id: fingerprint],
                days: 7,
                now: later(8 * 86400),
                archived: rig.backup.state.entries.compactMapValues(\.archivedFingerprint)
            )
        }
        // Mac에는 있지만 archive가 아직이면 사본이 Mac 하나뿐이다. 지우지 않는다
        #expect(cloud() == .uploading)
        #expect(expired().isEmpty)

        try rig.mac.archive(manifest)
        await rig.backup.runDue(now: later(3600))

        #expect(rig.backup.state.entries[manifest.id]?.archivedFingerprint == fingerprint)
        #expect(cloud() == .archived)
        #expect(expired() == [manifest.id])
    }

    @Test func 지문이_앞선_판이면_받은_원본만_기억하고_확인하지_않는다() async throws {
        let rig = Rig()
        var manifest = try rig.capture(try rig.newDocument(), byte: 1)
        await rig.backup.runDue()
        try rig.mac.importInbox(manifest.id)

        manifest = try rig.capture(manifest, byte: 2)
        await rig.backup.runDue(now: later(30))

        #expect(rig.host.confirmed.isEmpty)
        #expect(rig.backup.state.entries[manifest.id]?.macRaws == [manifest.pages[0].raw])
        #expect(rig.status(manifest) == .awaitingMac(.icloud))
    }

    @Test func Mac이_가져간_뒤의_다음_장은_새_원본만_올리고_manifest는_전체를_적는다() async throws {
        let rig = Rig()
        var manifest = try rig.capture(try rig.newDocument(), byte: 1)
        await rig.backup.runDue()
        try rig.mac.importInbox(manifest.id)
        await rig.backup.runDue(now: later(30))

        manifest = try rig.capture(manifest, byte: 2)
        await rig.backup.runDue(now: later(60))

        #expect(rig.inboxRaws(manifest) == [manifest.pages[1].raw])
        #expect(try rig.inboxManifest(manifest).pages.count == 2)
        // 앞 장의 원본 없이도 Mac이 합칠 수 있어야 한다(src-tauri/tests/inbox.rs)
        try rig.mac.importInbox(manifest.id)
        await rig.backup.runDue(now: later(90))
        guard case .sent = rig.status(manifest) else {
            Issue.record("Mac에 있음이어야 한다: \(rig.status(manifest))")
            return
        }
    }

    @Test func 다시_찍으면_새_원본을_올리고_옛_원본은_iCloud에서_지운다() async throws {
        let rig = Rig()
        var manifest = try rig.capture(try rig.newDocument(), byte: 1)
        manifest = try rig.capture(manifest, byte: 2)
        await rig.backup.runDue()
        let old = manifest.pages[0]

        manifest = try rig.capture(manifest, byte: 3, replacing: old.id)
        await rig.backup.runDue(now: later(30))

        #expect(rig.inboxRaws(manifest) == Set(manifest.pages.map(\.raw)))
        #expect(!rig.inboxRaws(manifest).contains(old.raw))
        #expect(try rig.inboxManifest(manifest) == manifest)
        #expect(try rig.inboxManifest(manifest).revision == manifest.revision)
    }

    @Test func 페이지를_지우면_iCloud에서도_빠지고_남은_원본은_그대로다() async throws {
        let rig = Rig()
        var manifest = try rig.capture(try rig.newDocument(), byte: 1)
        manifest = try rig.capture(manifest, byte: 2)
        await rig.backup.runDue()
        let removed = manifest.pages[1]

        manifest = try rig.store.deletePage(removed.id, from: manifest)
        rig.host.documents = [manifest]
        await rig.backup.runDue(now: later(30))

        #expect(rig.inboxRaws(manifest) == [manifest.pages[0].raw])
        #expect(try rig.inboxManifest(manifest) == manifest)
    }

    @Test func 받지_않은_문서의_장을_다_지우면_iCloud_사본을_치운다() async throws {
        let rig = Rig()
        var manifest = try rig.capture(try rig.newDocument(), byte: 1)
        await rig.backup.runDue()

        manifest = try rig.store.deletePage(manifest.pages[0].id, from: manifest)
        rig.host.documents = [manifest]
        await rig.backup.runDue(now: later(30))

        #expect(!FileManager.default.fileExists(atPath: rig.inbox(manifest).path))
        await rig.backup.runDue(now: later(60))
        #expect(!FileManager.default.fileExists(atPath: rig.inbox(manifest).path))
    }

    @Test func Mac이_받은_문서의_장을_다_지우면_빈_manifest를_알린다() async throws {
        let rig = Rig()
        var manifest = try rig.capture(try rig.newDocument(), byte: 1)
        await rig.backup.runDue()
        try rig.mac.importInbox(manifest.id)
        await rig.backup.runDue(now: later(30))

        manifest = try rig.store.deletePage(manifest.pages[0].id, from: manifest)
        rig.host.documents = [manifest]
        await rig.backup.runDue(now: later(60))

        #expect(try rig.inboxManifest(manifest).pages.isEmpty)
    }

    @Test func 문서를_지우면_iCloud_사본을_치우고_못_치우면_다시_켠_뒤에_이어서_치운다() async throws {
        let rig = Rig()
        let manifest = try rig.capture(try rig.newDocument(), byte: 1)
        await rig.backup.runDue()

        rig.rootFailure = ICloudFolderError.accessDenied
        rig.host.documents = []
        rig.backup.documentDeleted(manifest.id)
        await rig.backup.runDue()
        #expect(rig.backup.state.deletedDocuments == [manifest.id])
        #expect(FileManager.default.fileExists(atPath: rig.inbox(manifest).path))

        rig.rootFailure = nil
        rig.relaunch()
        #expect(rig.backup.state.deletedDocuments == [manifest.id])
        await rig.backup.runDue()

        #expect(rig.backup.state.deletedDocuments.isEmpty)
        #expect(!FileManager.default.fileExists(atPath: rig.inbox(manifest).path))
    }

    @Test func 복사가_실패해도_촬영은_되고_실패는_기다렸다_다시_시도하고_다시_켜면_바로_시도한다() async throws {
        let rig = Rig()
        // scanshelf 자리에 파일이 있어 폴더를 만들 수 없다
        try FileManager.default.createDirectory(at: rig.root.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data([0]).write(to: rig.root)

        var manifest = try rig.capture(try rig.newDocument(), byte: 1)
        await rig.backup.runDue()
        #expect(rig.backup.state.entries[manifest.id]?.attempts == 1)
        #expect(rig.backup.state.entries[manifest.id]?.lastError != nil)
        #expect(rig.status(manifest) == .backupPending)

        // 실패 중에도 찍힌다
        manifest = try rig.capture(manifest, byte: 2)
        #expect(manifest.pages.count == 2)

        // 기다리는 동안은 시도하지 않는다
        await rig.backup.runDue(now: later(1))
        #expect(rig.backup.state.entries[manifest.id]?.attempts == 1)
        await rig.backup.runDue(now: later(20))
        #expect(rig.backup.state.entries[manifest.id]?.attempts == 2)

        // 다시 켜면 실패 횟수는 남고 기다리지 않는다
        rig.relaunch()
        #expect(rig.backup.state.entries[manifest.id]?.attempts == 2)
        #expect(await rig.backup.pendingDocuments() == [manifest.id])
        try FileManager.default.removeItem(at: rig.root)
        await rig.backup.runDue()

        #expect(rig.backup.state.entries[manifest.id]?.attempts == 0)
        #expect(rig.backup.state.entries[manifest.id]?.lastError == nil)
        #expect(rig.inboxRaws(manifest) == Set(manifest.pages.map(\.raw)))
    }

    @Test func 앱이_복사_전에_죽어도_다시_켜면_밀린_문서를_올린다() async throws {
        let rig = Rig()
        let manifest = try rig.capture(try rig.newDocument(), byte: 1)

        rig.relaunch()

        #expect(await rig.backup.pendingDocuments() == [manifest.id])
        await rig.backup.runDue()
        #expect(await rig.backup.pendingDocuments().isEmpty)
        #expect(rig.inboxRaws(manifest) == [manifest.pages[0].raw])
    }

    @Test func 원본_복사_도중_죽었으면_manifest_없이_남고_다시_켜면_마저_올린다() async throws {
        let rig = Rig()
        var manifest = try rig.capture(try rig.newDocument(), byte: 1)
        manifest = try rig.capture(manifest, byte: 2)
        // 첫 장만 복사된 채 멈춘 모습
        let raw = rig.inbox(manifest).appending(path: manifest.pages[0].raw)
        try FileManager.default.createDirectory(at: raw.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data([1, 1]).write(to: raw)

        rig.relaunch()
        await rig.backup.runDue()

        #expect(rig.inboxRaws(manifest) == Set(manifest.pages.map(\.raw)))
        #expect(try rig.inboxManifest(manifest) == manifest)
    }

    @Test func 폴더를_고르지_않으면_아무것도_하지_않고_고르면_밀린_것을_올린다() async throws {
        let rig = Rig()
        rig.chosen = false
        let manifest = try rig.capture(try rig.newDocument(), byte: 1)

        await rig.backup.runDue()
        #expect(rig.backup.state.entries.isEmpty)
        #expect(rig.status(manifest) == .notSent)

        rig.chosen = true
        rig.backup.folderChanged()
        await rig.backup.runDue()
        #expect(rig.inboxRaws(manifest) == [manifest.pages[0].raw])
    }

    @Test func 폴더에_닿지_않으면_이유를_알린다() async throws {
        let rig = Rig()
        rig.rootFailure = ICloudFolderError.accessDenied

        await rig.backup.runDue()

        #expect(rig.backup.folderFailure == ICloudFolderError.accessDenied.localizedDescription)
    }

    @Test func Wi_Fi로_받으면_iCloud_사본을_치우고_그_원본은_다시_올리지_않는다() async throws {
        let rig = Rig()
        var manifest = try rig.capture(try rig.newDocument(), byte: 1)
        await rig.backup.runDue()
        #expect(FileManager.default.fileExists(atPath: rig.inbox(manifest).path))

        rig.host.records[manifest.id] = SendRecord(method: .wifi, sentAt: .now, manifestFingerprint: try manifest.fingerprint(), confirmed: true)
        rig.backup.noteDelivered(manifest.id, manifest: manifest)
        await rig.backup.runDue(now: later(1))

        #expect(!FileManager.default.fileExists(atPath: rig.inbox(manifest).path))
        #expect(rig.host.confirmed.isEmpty)

        manifest = try rig.capture(manifest, byte: 2)
        await rig.backup.runDue(now: later(30))
        #expect(rig.inboxRaws(manifest) == [manifest.pages[1].raw])
    }

    @Test func 빈_확인_파일을_받으면_Mac에_있다던_원본까지_다시_올린다() async throws {
        let rig = Rig()
        var manifest = try rig.capture(try rig.newDocument(), byte: 1)
        await rig.backup.runDue()
        try rig.mac.importInbox(manifest.id)
        await rig.backup.runDue(now: later(30))

        manifest = try rig.capture(manifest, byte: 2)
        await rig.backup.runDue(now: later(60))
        #expect(rig.inboxRaws(manifest) == [manifest.pages[1].raw])

        // Mac에서 문서를 지웠다
        try rig.mac.writeReceipt(manifest.id, sha256: nil, raw: [])
        await rig.backup.runDue(now: later(300))

        #expect(rig.backup.state.entries[manifest.id]?.macRaws.isEmpty == true)
        #expect(rig.inboxRaws(manifest) == Set(manifest.pages.map(\.raw)))
    }
}

struct BackupQueueTests {
    private let record = SendRecord(method: .wifi, sentAt: .now, manifestFingerprint: "a", confirmed: true)

    @Test func 안_올린_판은_올리고_올린_판은_확인을_기다리고_확인되면_치운다() {
        var entry = BackupEntry()
        #expect(BackupQueue.work(hasPages: true, fingerprint: "a", record: nil, entry: nil) == .copy)
        #expect(BackupQueue.work(hasPages: true, fingerprint: "a", record: nil, entry: entry) == .copy)

        entry.copiedFingerprint = "a"
        #expect(BackupQueue.work(hasPages: true, fingerprint: "a", record: nil, entry: entry) == .watch)
        #expect(BackupQueue.work(hasPages: true, fingerprint: "b", record: nil, entry: entry) == .copy)
        #expect(BackupQueue.work(hasPages: true, fingerprint: "a", record: record, entry: entry) == .cleanup)

        entry.copiedFingerprint = nil
        #expect(BackupQueue.work(hasPages: true, fingerprint: "a", record: record, entry: entry) == .verifyArchive)

        entry.archivedFingerprint = "a"
        #expect(BackupQueue.work(hasPages: true, fingerprint: "a", record: record, entry: entry) == .none)
    }

    @Test func archive_확인_간격은_두_배씩_늘고_1시간에서_멈춘다() {
        #expect(BackupQueue.archiveCheckDelay(checks: 0) == 20)
        #expect(BackupQueue.archiveCheckDelay(checks: 2) == 80)
        #expect(BackupQueue.archiveCheckDelay(checks: 20) == 3600)
    }

    @Test func Wi_Fi로_받은_뒤_바뀐_판은_다시_올린다() {
        #expect(BackupQueue.work(hasPages: true, fingerprint: "b", record: record, entry: nil) == .copy)
    }

    @Test func 빈_문서는_올릴_것이_없고_Mac이_아는_문서는_빈_판도_알린다() {
        #expect(BackupQueue.work(hasPages: false, fingerprint: "a", record: nil, entry: nil) == .none)
        #expect(BackupQueue.work(hasPages: false, fingerprint: "a", record: nil, entry: BackupEntry()) == .none)
        #expect(BackupQueue.work(hasPages: false, fingerprint: "a", record: nil, entry: BackupEntry(macRaws: ["raw/a.heic"])) == .copy)
    }

    @Test func 실패할수록_오래_기다리되_1시간을_넘지_않는다() {
        #expect(BackupQueue.retryDelay(attempts: 1) == 15)
        #expect(BackupQueue.retryDelay(attempts: 2) == 30)
        #expect(BackupQueue.retryDelay(attempts: 100) == 3600)
        #expect(BackupQueue.recopyGrace(recopies: 0) == 120)
        #expect(BackupQueue.recopyGrace(recopies: 1) == 240)
        #expect(BackupQueue.recopyGrace(recopies: 100) == 3600)
    }

    @Test func 기록을_저장했다_읽으면_같고_깨진_파일은_빈_기록으로_시작한다() throws {
        let url = FileManager.default.temporaryDirectory.appending(path: "scanshelf-state-\(UUID().uuidString)/state.json")
        defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
        let stateStore = BackupStateStore(url: url)
        let id = UUID()
        var state = BackupState()
        state.entries[id] = BackupEntry(
            copiedFingerprint: "a",
            uploadedFingerprint: "a",
            copiedAt: Date(timeIntervalSince1970: 1_800_000_000),
            macRaws: ["raw/a.heic"],
            attempts: 2,
            recopies: 1,
            lastError: "실패"
        )
        state.deletedDocuments = [UUID()]

        try stateStore.save(state)
        #expect(stateStore.load() == state)

        try Data("깨짐".utf8).write(to: url)
        #expect(stateStore.load() == BackupState())
    }
}

struct DocumentProtectionStateTests {
    private func resolve(
        record: SendRecord? = nil,
        fingerprint: String = "a",
        hasPages: Bool = true,
        folderChosen: Bool = true,
        backup: BackupEntry? = nil,
        failure: String? = nil
    ) -> SendStatus {
        SendStatus.resolve(record: record, fingerprint: fingerprint, hasPages: hasPages, folderChosen: folderChosen, backup: backup, transferFailure: failure)
    }

    @Test func 복사_전이나_올라가기_전은_백업_대기다() {
        #expect(resolve() == .backupPending)
        #expect(resolve(backup: BackupEntry(copiedFingerprint: "a")) == .backupPending)
        #expect(resolve(backup: BackupEntry(uploadedFingerprint: "old")) == .backupPending)
    }

    @Test func 지금_판이_올라갔으면_iCloud에_있음이다() {
        #expect(resolve(backup: BackupEntry(uploadedFingerprint: "a")) == .awaitingMac(.icloud))
    }

    @Test func Mac이_지금_판을_확인했으면_Mac에_있음이다() {
        let at = Date(timeIntervalSince1970: 1_800_000_000)
        let record = SendRecord(method: .icloud, sentAt: at, manifestFingerprint: "a", confirmed: true)

        #expect(resolve(record: record) == .sent(.icloud, at))
        #expect(resolve(record: record, folderChosen: false) == .sent(.icloud, at))
    }

    @Test func Mac이_확인한_뒤_더_찍으면_다시_백업_대기다() {
        let record = SendRecord(method: .wifi, sentAt: .now, manifestFingerprint: "a", confirmed: true)

        #expect(resolve(record: record, fingerprint: "b") == .backupPending)
    }

    @Test func 폴더가_없으면_이_iPhone에만_있는_상태다() {
        let record = SendRecord(method: .wifi, sentAt: .now, manifestFingerprint: "a", confirmed: true)

        #expect(resolve(folderChosen: false) == .notSent)
        #expect(resolve(record: record, fingerprint: "b", folderChosen: false) == .changedSinceSent)
    }

    @Test func Wi_Fi_실패는_iCloud가_받쳐_주면_드러내지_않는다() {
        #expect(resolve(failure: "실패") == .backupPending)
        #expect(resolve(folderChosen: false, failure: "실패") == .failed("실패"))
    }

    @Test func 빈_문서는_백업_대기가_아니다() {
        #expect(resolve(hasPages: false) == .notSent)
    }

    @Test func 폴더가_없거나_닿지_않을_때만_안내한다() {
        #expect(BackupNotice.of(folderChosen: false, folderFailure: nil) == .folderNotChosen)
        #expect(BackupNotice.of(folderChosen: true, folderFailure: "x") == .folderUnavailable("x"))
        #expect(BackupNotice.of(folderChosen: true, folderFailure: nil) == nil)
    }
}

struct ManifestRevisionTests {
    private let store = DocumentStore(root: FileManager.default.temporaryDirectory.appending(path: "scanshelf-revision-\(UUID().uuidString)"))

    @Test func 저장할_때마다_번호가_오른다() throws {
        var manifest = try store.create(name: "예시_1", kind: .document, session: nil)
        #expect(manifest.revision == 1)

        manifest = try store.addPage(to: manifest, photo: Data([1]), quad: nil, layout: .single, capturedAt: .now)
        #expect(manifest.revision == 2)
        manifest = try store.addPage(to: manifest, photo: Data([2]), quad: nil, layout: .single, capturedAt: .now, replacing: manifest.pages[0].id)
        #expect(manifest.revision == 3)
        manifest = try store.updateMetadata(of: manifest, name: "새 이름", kind: .book, session: nil)
        #expect(manifest.revision == 4)
        manifest = try store.deletePage(manifest.pages[0].id, from: manifest)
        #expect(manifest.revision == 5)
        #expect(try store.load(manifest.id) == manifest)
    }

    @Test func 옛_값을_저장해도_번호는_디스크보다_커진다() throws {
        let stale = try store.create(name: "예시_1", kind: .document, session: nil)
        let newer = try store.addPage(to: stale, photo: Data([1]), quad: nil, layout: .single, capturedAt: .now)

        let saved = try store.updateMetadata(of: stale, name: "새 이름", kind: .document, session: nil)

        #expect(saved.revision == (newer.revision ?? 0) + 1)
    }

    @Test func 번호가_없는_옛_manifest도_읽는다() throws {
        let id = UUID()
        let directory = store.directory(of: id)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let text = "{\"schemaVersion\":1,\"id\":\"\(id.uuidString.lowercased())\",\"name\":\"예시\",\"kind\":\"document\",\"createdAt\":\"2026-11-05T01:20:03.412Z\",\"pages\":[]}"
        try Data(text.utf8).write(to: directory.appending(path: "manifest.json"))

        #expect(try store.load(id).revision == nil)
        #expect(try store.updateMetadata(of: try store.load(id), name: "새 이름", kind: .document, session: nil).revision == 1)
    }

    @Test func 지문은_저장된_바이트의_해시와_같다() throws {
        // Mac이 확인 파일에 적는 값이 저장된 바이트의 해시다. 달라지면 영영 확인되지 않는다
        let manifest = try store.addPage(
            to: try store.create(name: "예시_1", kind: .document, session: "예시"),
            photo: Data([1]),
            quad: nil,
            layout: .single,
            capturedAt: Date(timeIntervalSince1970: 1_800_000_000.412)
        )

        let bytes = try Data(contentsOf: store.manifestURL(of: manifest.id))

        #expect(SHA256.hash(data: bytes).hexString == (try manifest.fingerprint()))
    }
}

struct CloudStatusTests {
    @Test func 장소별_상태를_따로_센다() {
        let counts = StorageCounts.tally([
            (.sent(.wifi, .now), .archived),
            (.sent(.icloud, .now), .uploading),
            (.awaitingMac(.icloud), .inInbox),
            (.backupPending, .uploading),
            (.notSent, .off),
        ])
        #expect(counts.total == 5)
        #expect(counts.onMac == 2)
        #expect(counts.viaWiFi == 1)
        #expect(counts.archived == 1)
        #expect(counts.inInbox == 1)
        #expect(counts.uploading == 2)
        // Mac에 있으면 앱을 지워도 남는다. inbox에 올라간 것도 남는다
        #expect(counts.onlyHere == 2)
    }

    @Test func Mac이_받으면_inbox_상태가_아니라_archive를_기다린다() {
        let delivered = SendRecord(method: .wifi, sentAt: .now, manifestFingerprint: "a", confirmed: true)
        let uploaded = BackupEntry(uploadedFingerprint: "a")
        #expect(CloudStatus.resolve(folderChosen: true, hasPages: true, fingerprint: "a", record: nil, backup: uploaded) == .inInbox)
        #expect(CloudStatus.resolve(folderChosen: true, hasPages: true, fingerprint: "a", record: delivered, backup: uploaded) == .uploading)
        #expect(CloudStatus.resolve(folderChosen: false, hasPages: true, fingerprint: "a", record: delivered, backup: uploaded) == .off)
    }
}
