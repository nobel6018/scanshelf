import Foundation
import UIKit

// 찍은 장을 바로 iCloud 폴더로 복사하고, Mac이 받았다는 확인 파일을 기다린다.
// 복사는 메인 스레드 밖에서 한 번에 한 문서씩 한다. 셔터는 이 일을 기다리지 않고, 실패해도 촬영에 영향이 없다.
// Mac이 받았는지는 received/<문서 id>.json으로만 판단한다. inbox 폴더가 사라진 것은 근거가 되지 않는다.
// Mac이 가져가며 지운 사이 iPhone이 올린 다음 장이 같이 사라질 수 있기 때문이다.
// @see docs/transfer.md

/// Mac의 `src-tauri/src/inbox.rs` `Receipt`와 같은 모양
struct MacReceipt: Codable, Equatable, Sendable {
    var documentId: UUID
    /// 가져온 manifest 바이트의 SHA-256
    var manifestSha256: String?
    /// 보관함에 있는 원본 전부. 비어 있으면 Mac에서 문서를 지웠으니 처음부터 다시 올린다
    var raw: [String]
}

struct BackupSyncOutcome: Equatable, Sendable {
    var entry: BackupEntry
    /// 이번에 Mac이 받았다고 확인된 지문
    var confirmedFingerprint: String?
}

/// 파일 일. 오래 막힐 수 있으니(NSFileCoordinator, iCloud) 메인 스레드 밖에서 부른다
enum ICloudBackupEngine {
    /// 문서 하나를 지금 판에 맞춘다. 여러 번 불러도 같은 결과가 되고, 중간에 멈추면 다음에 이어 간다
    static func sync(
        documentID: UUID,
        store: DocumentStore,
        scanshelfRoot root: URL,
        entry previous: BackupEntry,
        confirmedFingerprint: String?,
        now: Date
    ) throws -> BackupSyncOutcome {
        // 디스크의 manifest가 기준이다. 원본을 먼저 쓰고 manifest를 쓰므로 여기 적힌 원본은 항상 있다
        let manifestData = try Data(contentsOf: store.manifestURL(of: documentID))
        let manifest = try Manifest.decoder().decode(Manifest.self, from: manifestData)
        try manifest.validate()
        let fingerprint = try manifest.fingerprint()
        let target = ICloudFolderLayout.inbox(in: root, documentID: documentID)
        var entry = previous

        var confirmed = confirmedFingerprint == fingerprint
        var newlyConfirmed: String?
        if let receipt = readReceipt(root: root, documentID: documentID) {
            entry.macRaws = receipt.raw.isEmpty ? [] : entry.macRaws.union(receipt.raw)
            if receipt.manifestSha256 == fingerprint {
                if !confirmed { newlyConfirmed = fingerprint }
                confirmed = true
            }
        }

        if confirmed {
            entry.macRaws.formUnion(manifest.pages.map(\.raw))
            try remove(target)
            entry = entry.withoutCopy()
            if entry.archivedFingerprint != fingerprint, !manifest.pages.isEmpty {
                if isArchived(manifest: manifest, archive: ICloudFolderLayout.archive(in: root, documentID: documentID)) {
                    entry.archivedFingerprint = fingerprint
                    entry.archiveChecks = nil
                } else {
                    entry.archiveChecks = (entry.archiveChecks ?? 0) + 1
                }
            }
            return BackupSyncOutcome(entry: entry, confirmedFingerprint: newlyConfirmed)
        }
        // 찍었던 장을 다 지운 문서는 Mac이 모르면 알릴 것이 없다
        if manifest.pages.isEmpty, entry.macRaws.isEmpty {
            try remove(target)
            return BackupSyncOutcome(entry: entry.withoutCopy(), confirmedFingerprint: nil)
        }

        let isNewVersion = entry.copiedFingerprint != fingerprint
        if isNewVersion || shouldCopyAgain(manifest: manifest, entry: entry, target: target, now: now) {
            entry.recopies = isNewVersion ? 0 : entry.recopies + 1
            try copy(manifest: manifest, manifestData: manifestData, documentID: documentID, store: store, skipping: entry.macRaws, to: target)
            entry.copiedFingerprint = fingerprint
            entry.copiedAt = now
        }
        entry.attempts = 0
        entry.lastError = nil
        if isUploaded(manifest: manifest, target: target) {
            entry.uploadedFingerprint = fingerprint
        }
        return BackupSyncOutcome(entry: entry, confirmedFingerprint: nil)
    }

    /// Mac이 archive에 manifest와 지금 판의 원본을 다 넣었는지. 다른 기기가 올린 파일은 iCloud 서버에 올라간 뒤에야
    /// 이 iPhone 목록에 보이므로, 보이면 iCloud에 있는 것이다. 내용은 내려받지 않는다(원본은 이름이 페이지 id라 바뀌지 않는다)
    static func isArchived(manifest: Manifest, archive: URL) -> Bool {
        // 한 번도 열어 보지 않은 iCloud 폴더는 목록이 비어 보일 수 있다. 메타데이터만 읽게 조정해 목록을 채운다
        var coordinationError: NSError?
        NSFileCoordinator(filePresenter: nil).coordinate(
            readingItemAt: archive,
            options: [.immediatelyAvailableMetadataOnly],
            error: &coordinationError
        ) { url in
            _ = try? FileManager.default.contentsOfDirectory(atPath: url.appending(path: DocumentStore.rawDirectory).path)
        }
        let files = [archive.appending(path: ManifestSchema.fileName)] + manifest.pages.map { archive.appending(path: $0.raw) }
        return files.allSatisfy(isPresent)
    }

    /// 올렸는데 폴더에서 빠진 것이 있고, Mac에 있다는 확인도 없으면 시간을 두고 다시 올린다
    private static func shouldCopyAgain(manifest: Manifest, entry: BackupEntry, target: URL, now: Date) -> Bool {
        let manifestGone = !isPresent(target.appending(path: ManifestSchema.fileName))
        let rawGone = manifest.pages.contains { !entry.macRaws.contains($0.raw) && !isPresent(target.appending(path: $0.raw)) }
        guard manifestGone || rawGone else { return false }
        let copiedAt = entry.copiedAt ?? .distantPast
        return now.timeIntervalSince(copiedAt) >= BackupQueue.recopyGrace(recopies: entry.recopies)
    }

    private static func copy(
        manifest: Manifest,
        manifestData: Data,
        documentID: UUID,
        store: DocumentStore,
        skipping onMac: Set<String>,
        to target: URL
    ) throws {
        try FileManager.default.createDirectory(at: target, withIntermediateDirectories: true)
        for page in manifest.pages where !onMac.contains(page.raw) {
            let source = store.fileURL(of: documentID, relativePath: page.raw)
            let destination = target.appending(path: page.raw)
            // 원본 파일은 페이지 id로 이름이 정해지고 바뀌지 않는다. 같은 크기면 이미 끝난 복사다
            if sameSize(source, destination) || isPlaceholderPresent(destination) { continue }
            try FileManager.default.createDirectory(at: destination.deletingLastPathComponent(), withIntermediateDirectories: true)
            try coordinatedWrite(to: destination) { url in
                try DocumentStore.writeAtomically(try Data(contentsOf: source, options: .mappedIfSafe), to: url)
            }
        }
        try coordinatedWrite(to: target.appending(path: ManifestSchema.fileName)) { url in
            try DocumentStore.writeAtomically(manifestData, to: url)
        }
        // manifest를 바꾼 뒤에 옛 파일을 지운다. 반대로 하면 Mac이 원본이 빠진 옛 manifest를 보고 기다리게 된다
        try prune(target: target, keeping: Set(manifest.pages.map(\.raw)))
    }

    /// 다시 찍거나 지운 장의 원본처럼 지금 판에 없는 파일을 치운다. 이 일꾼이 유일한 쓰는 쪽이라 쓰는 중인 임시 파일은 없다
    private static func prune(target: URL, keeping wanted: Set<String>) throws {
        let fileManager = FileManager.default
        let rawDirectory = target.appending(path: DocumentStore.rawDirectory, directoryHint: .isDirectory)
        for name in (try? fileManager.contentsOfDirectory(atPath: rawDirectory.path)) ?? [] {
            let realName = placeholderTarget(of: name) ?? name
            if wanted.contains("\(DocumentStore.rawDirectory)/\(realName)") { continue }
            try remove(rawDirectory.appending(path: name))
        }
        // iCloud가 충돌을 풀며 만든 사본(`manifest 2.json` 등)이 폴더를 비지 않게 막지 않도록 같이 치운다
        for name in (try? fileManager.contentsOfDirectory(atPath: target.path)) ?? [] {
            if name == DocumentStore.rawDirectory || (placeholderTarget(of: name) ?? name) == ManifestSchema.fileName { continue }
            try remove(target.appending(path: name))
        }
    }

    /// 지운 문서나 Mac이 받은 문서의 iCloud 사본을 지운다. 없으면 할 일이 없다
    static func remove(_ url: URL) throws {
        guard FileManager.default.fileExists(atPath: url.path) else { return }
        var coordinationError: NSError?
        var removeError: Error?
        NSFileCoordinator(filePresenter: nil).coordinate(writingItemAt: url, options: .forDeleting, error: &coordinationError) { target in
            do {
                try FileManager.default.removeItem(at: target)
            } catch CocoaError.fileNoSuchFile {
                return
            } catch {
                removeError = error
            }
        }
        if let coordinationError { throw coordinationError }
        if let removeError { throw removeError }
    }

    /// 확인 파일을 읽는다. 아직 내려오지 않았으면 받아 달라고만 하고 다음에 다시 본다. 기다리며 막히지 않는다
    static func readReceipt(root: URL, documentID: UUID) -> MacReceipt? {
        let url = ICloudFolderLayout.receipt(in: root, documentID: documentID)
        if isPlaceholderPresent(url) {
            try? FileManager.default.startDownloadingUbiquitousItem(at: url)
            return nil
        }
        guard FileManager.default.fileExists(atPath: url.path) else { return nil }
        let status = (try? url.resourceValues(forKeys: [.ubiquitousItemDownloadingStatusKey]))?.ubiquitousItemDownloadingStatus
        if status == .notDownloaded {
            try? FileManager.default.startDownloadingUbiquitousItem(at: url)
            return nil
        }
        var data: Data?
        var coordinationError: NSError?
        NSFileCoordinator(filePresenter: nil).coordinate(readingItemAt: url, options: .withoutChanges, error: &coordinationError) { target in
            data = try? Data(contentsOf: target)
        }
        guard let data, let receipt = try? JSONDecoder().decode(MacReceipt.self, from: data), receipt.documentId == documentID else {
            return nil
        }
        return receipt
    }

    /// iCloud가 다 올렸다고 하는지. iCloud 밖의 파일(시험용 임시 폴더)은 값이 없어 올라간 것으로 본다.
    /// Mac이 이미 가져가 사라진 파일도 올라갔던 것이다
    private static func isUploaded(manifest: Manifest, target: URL) -> Bool {
        let files = [target.appending(path: ManifestSchema.fileName)] + manifest.pages.map { target.appending(path: $0.raw) }
        return files.allSatisfy { url in
            guard FileManager.default.fileExists(atPath: url.path) else { return true }
            return (try? url.resourceValues(forKeys: [.ubiquitousItemIsUploadedKey]))?.ubiquitousItemIsUploaded ?? true
        }
    }

    private static func isPresent(_ url: URL) -> Bool {
        FileManager.default.fileExists(atPath: url.path) || isPlaceholderPresent(url)
    }

    /// 옛 방식의 자리표시 `.이름.icloud`. iCloud가 내용을 내려 두지 않은 파일이다
    private static func isPlaceholderPresent(_ url: URL) -> Bool {
        FileManager.default.fileExists(atPath: url.deletingLastPathComponent().appending(path: ".\(url.lastPathComponent).icloud").path)
    }

    private static func placeholderTarget(of name: String) -> String? {
        guard name.hasPrefix("."), name.hasSuffix(".icloud") else { return nil }
        return String(name.dropFirst().dropLast(".icloud".count))
    }

    private static func sameSize(_ a: URL, _ b: URL) -> Bool {
        guard let sizeA = try? a.resourceValues(forKeys: [.fileSizeKey]).fileSize,
              let sizeB = try? b.resourceValues(forKeys: [.fileSizeKey]).fileSize
        else { return false }
        return sizeA == sizeB
    }

    private static func coordinatedWrite(to url: URL, _ write: (URL) throws -> Void) throws {
        var coordinationError: NSError?
        var writeError: Error?
        NSFileCoordinator(filePresenter: nil).coordinate(writingItemAt: url, options: .forReplacing, error: &coordinationError) { target in
            do { try write(target) } catch { writeError = error }
        }
        if let coordinationError { throw coordinationError }
        if let writeError { throw writeError }
    }
}

extension BackupEntry {
    fileprivate func withoutCopy() -> BackupEntry {
        var entry = self
        entry.copiedFingerprint = nil
        entry.uploadedFingerprint = nil
        entry.copiedAt = nil
        entry.attempts = 0
        entry.recopies = 0
        entry.lastError = nil
        return entry
    }
}

/// 일꾼이 앱에서 읽고 알리는 것
@MainActor
protocol BackupHost: AnyObject {
    var documents: [Manifest] { get }
    var records: [UUID: SendRecord] { get }
    func backupConfirmed(_ documentID: UUID, fingerprint: String)
}

@MainActor
@Observable
final class ICloudBackup {
    private(set) var state: BackupState
    /// 폴더를 골랐는데 닿지 않을 때의 이유
    private(set) var folderFailure: String?

    @ObservationIgnored weak var host: (any BackupHost)?

    @ObservationIgnored private let store: DocumentStore
    @ObservationIgnored private let stateStore: BackupStateStore
    /// false면 `kick`이 일을 돌리지 않는다. 시험이 `runDue`를 직접 불러 순서를 정하려고 둔다
    @ObservationIgnored private let automatic: Bool
    /// 고른 폴더의 scanshelf 루트. 고르지 않았으면 nil
    @ObservationIgnored private let root: @MainActor () throws -> URL?
    /// 문서마다 다음에 볼 시각. 저장하지 않아 앱을 다시 켜면 실패했던 것도 바로 다시 시도한다
    @ObservationIgnored private var notBefore: [UUID: Date] = [:]
    @ObservationIgnored private var loop: Task<Void, Never>?
    @ObservationIgnored private var sleeper: Task<Void, Never>?
    @ObservationIgnored private var runAgain = false
    @ObservationIgnored private var backgroundTask = UIBackgroundTaskIdentifier.invalid

    init(store: DocumentStore, stateStore: BackupStateStore, automatic: Bool = true, root: @escaping @MainActor () throws -> URL?) {
        self.store = store
        self.stateStore = stateStore
        self.automatic = automatic
        self.root = root
        state = stateStore.load()
    }

    /// 깨운다. 바로 돌아온다. id를 주면 그 문서를, 없으면 모든 문서를 기다리지 않고 본다
    func kick(_ documentID: UUID? = nil) {
        if let documentID {
            notBefore[documentID] = nil
        } else {
            notBefore = [:]
        }
        guard automatic else { return }
        runAgain = true
        sleeper?.cancel()
        guard loop == nil else { return }
        loop = Task { [weak self] in
            await self?.runLoop()
        }
    }

    /// 폴더를 바꾸거나 잊었다. 복사해 둔 기록과 보관 확인은 그 폴더 이야기라 지운다. Mac에 있다는 기록은 그대로 둔다
    func folderChanged() {
        folderFailure = nil
        for id in state.entries.keys {
            state.entries[id] = state.entries[id]?.withoutCopy()
            state.entries[id]?.archivedFingerprint = nil
            state.entries[id]?.archiveChecks = nil
        }
        saveState()
        kick()
    }

    /// Wi-Fi로 Mac이 받았다
    func noteDelivered(_ documentID: UUID, manifest: Manifest) {
        var entry = state.entries[documentID] ?? BackupEntry()
        entry.macRaws.formUnion(manifest.pages.map(\.raw))
        state.entries[documentID] = entry
        saveState()
        kick(documentID)
    }

    /// iPhone에서 문서를 지웠다. Mac이 뒤늦게 가져가지 않게 iCloud 사본도 치운다
    func documentDeleted(_ documentID: UUID) {
        state.entries[documentID] = nil
        notBefore[documentID] = nil
        do {
            if try root() != nil { state.deletedDocuments.insert(documentID) }
        } catch {
            state.deletedDocuments.insert(documentID)
        }
        saveState()
        kick()
    }

    /// 지금 볼 차례인 문서를 한 번씩 처리한다
    func runDue(now: Date = .now) async {
        let scanshelfRoot: URL
        do {
            guard let resolved = try root() else {
                folderFailure = nil
                return
            }
            scanshelfRoot = resolved
            folderFailure = nil
        } catch {
            folderFailure = error.localizedDescription
            return
        }
        await removeDeletedDocuments(root: scanshelfRoot)
        let due = await plan().map(\.id).filter { (notBefore[$0] ?? .distantPast) <= now }
        for id in due {
            await sync(id, root: scanshelfRoot, now: now)
        }
    }

    private func runLoop() async {
        repeat {
            runAgain = false
            beginBackgroundTime()
            await runDue()
            endBackgroundTime()
            if runAgain { continue }
            guard let delay = await nextDelay(now: .now) else { break }
            let sleeping = Task {
                _ = try? await Task.sleep(for: .seconds(max(delay, 1)))
            }
            sleeper = sleeping
            await sleeping.value
            sleeper = nil
        } while true
        loop = nil
    }

    private func plan() async -> [BackupItem] {
        let documents = host?.documents ?? []
        let records = host?.records ?? [:]
        let entries = state.entries
        return await offMain {
            BackupQueue.plan(documents: documents, records: records, entries: entries)
        }
    }

    /// 올려야 할 문서. 앱을 다시 켜면 이 값이 그대로 밀린 일이 된다
    func pendingDocuments() async -> [UUID] {
        await plan().filter { $0.work == .copy }.map(\.id)
    }

    /// 다음에 깨어날 때까지의 시간. 볼 것이 없으면 멈추고 다음 `kick`을 기다린다
    private func nextDelay(now: Date) async -> TimeInterval? {
        if folderFailure != nil { return BackupQueue.folderRetryDelay }
        // 폴더를 고르지 않았으면 할 수 있는 일이 없다. 고르면 다시 깨운다
        guard (try? root()) != nil else { return nil }
        if !state.deletedDocuments.isEmpty { return BackupQueue.folderRetryDelay }
        let times = await plan().map { notBefore[$0.id] ?? now }
        return times.min().map { $0.timeIntervalSince(now) }
    }

    private func sync(_ id: UUID, root: URL, now: Date) async {
        guard let host else { return }
        let before = state.entries[id] ?? BackupEntry()
        let confirmed = host.records[id].flatMap { $0.confirmed ? $0.manifestFingerprint : nil }
        let wanted = host.documents.first { $0.id == id }.flatMap { try? $0.fingerprint() }
        let store = store
        let result = await offMain {
            Result {
                try ICloudBackupEngine.sync(documentID: id, store: store, scanshelfRoot: root, entry: before, confirmedFingerprint: confirmed, now: now)
            }
        }
        // 복사하는 사이 문서를 지웠으면 기록을 남기지 않는다. 남은 파일은 지운 문서 치우기가 처리한다
        guard host.documents.contains(where: { $0.id == id }) else { return }
        switch result {
        case let .success(outcome):
            var entry = outcome.entry
            // 복사하는 사이 Wi-Fi로 받은 원본 기록을 잃지 않는다
            entry.macRaws.formUnion((state.entries[id]?.macRaws ?? []).subtracting(before.macRaws))
            state.entries[id] = entry
            // Mac이 받았는데 archive가 아직이면 간격을 늘려 가며 본다. Mac이 옛 버전이면 영영 안 생기므로 계속 20초로 보지 않는다
            let waitingArchive = confirmed != nil && confirmed == wanted && entry.archivedFingerprint != wanted
            notBefore[id] = now.addingTimeInterval(
                waitingArchive ? BackupQueue.archiveCheckDelay(checks: max((entry.archiveChecks ?? 0) - 1, 0)) : BackupQueue.watchInterval
            )
            if let fingerprint = outcome.confirmedFingerprint {
                host.backupConfirmed(id, fingerprint: fingerprint)
            }
        case let .failure(error):
            // 복사하는 사이 그 장을 지웠거나 다시 찍었으면 실패가 아니다. 새 판으로 바로 다시 한다
            if (try? store.load(id).fingerprint()) != wanted {
                notBefore[id] = nil
                runAgain = true
                return
            }
            var entry = state.entries[id] ?? before
            entry.attempts += 1
            entry.lastError = error.localizedDescription
            state.entries[id] = entry
            notBefore[id] = now.addingTimeInterval(BackupQueue.retryDelay(attempts: entry.attempts))
        }
        await saveStateOffMain()
    }

    private func removeDeletedDocuments(root: URL) async {
        guard !state.deletedDocuments.isEmpty else { return }
        for id in state.deletedDocuments {
            let target = ICloudFolderLayout.inbox(in: root, documentID: id)
            let removed = await offMain { (try? ICloudBackupEngine.remove(target)) != nil }
            if removed { state.deletedDocuments.remove(id) }
        }
        await saveStateOffMain()
    }

    /// 페이지가 많으면 기록도 커진다. 찍는 동안 불리니 메인 스레드를 잡지 않는다
    private func saveStateOffMain() async {
        let stateStore = stateStore
        let snapshot = state
        await offMain {
            try? stateStore.save(snapshot)
        }
    }

    private func saveState() {
        // 저장에 실패해도 다음에 다시 계산한다. 최악은 같은 파일을 한 번 더 살펴보는 것이다
        try? stateStore.save(state)
    }

    /// 앱을 내려도 복사하던 파일은 마칠 수 있게 시간을 얻는다. 그 뒤는 다음에 앞으로 올 때 이어 간다
    private func beginBackgroundTime() {
        backgroundTask = UIApplication.shared.beginBackgroundTask(withName: "scanshelf.backup") { [weak self] in
            self?.endBackgroundTime()
        }
    }

    private func endBackgroundTime() {
        guard backgroundTask != .invalid else { return }
        UIApplication.shared.endBackgroundTask(backgroundTask)
        backgroundTask = .invalid
    }
}

/// 파일 조정(NSFileCoordinator)은 스레드를 오래 잡을 수 있어 Swift 동시성 스레드 풀 밖에서 돌린다
@discardableResult
private func offMain<T: Sendable>(_ body: @escaping @Sendable () -> T) async -> T {
    await withCheckedContinuation { continuation in
        DispatchQueue.global(qos: .utility).async {
            continuation.resume(returning: body())
        }
    }
}
