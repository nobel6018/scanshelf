import Foundation

// 앱을 지워도 스캔이 남도록 찍을 때마다 복사하는 곳. 무료 Apple ID 서명이라 CloudKit과 앱 전용 iCloud 폴더를 못 쓴다.
// 파일 앱 폴더 선택기로 iCloud Drive/scanshelf를 한 번 고르고 보안 범위 북마크로 기억한다.
// 복사는 Transfer/ICloudBackup.swift가 한다.
// @see docs/transfer.md

enum ICloudFolderError: Error, LocalizedError {
    case notICloudFolder
    case accessDenied
    case notChosen

    var errorDescription: String? {
        switch self {
        case .notICloudFolder: "iCloud Drive의 scanshelf 폴더(또는 iCloud Drive 자체)를 골라 주세요"
        case .accessDenied: "iCloud 폴더에 접근하지 못했습니다. 설정에서 폴더를 다시 골라 주세요"
        case .notChosen: "iCloud 폴더를 아직 고르지 않았습니다"
        }
    }
}

enum ICloudFolderLayout {
    static let rootName = "scanshelf"
    static let inboxName = "inbox"
    static let receivedName = "received"
    /// Mac이 받은 원본을 영구 보관하는 곳. Mac만 쓴다(src-tauri/src/archive.rs)
    static let archiveName = "archive"
    private static let iCloudDriveName = "com~apple~CloudDocs"

    /// 고른 폴더에서 scanshelf 루트를 찾는다. scanshelf 안의 inbox나 문서 폴더를 고르는 실수를 받아 주려고 위로 올라가며 찾는다.
    /// iCloud Drive 자체를 고르면 그 안의 scanshelf를 쓴다. 그 밖(이 iPhone 안 등)은 Mac이 볼 수 없으니 거절한다
    static func scanshelfRoot(forPicked picked: URL) -> URL? {
        let standardized = picked.standardizedFileURL
        var current = standardized
        while current.pathComponents.count > 1 {
            if current.lastPathComponent == iCloudDriveName { break }
            if current.lastPathComponent == rootName, isInsideICloudDrive(current) { return current }
            current = current.deletingLastPathComponent()
        }
        if standardized.lastPathComponent == iCloudDriveName {
            return standardized.appending(path: rootName, directoryHint: .isDirectory)
        }
        return nil
    }

    static func inbox(in root: URL, documentID: UUID) -> URL {
        root.appending(path: inboxName, directoryHint: .isDirectory)
            .appending(path: documentID.uuidString.lowercased(), directoryHint: .isDirectory)
    }

    /// Mac이 받은 것을 적어 두는 파일
    static func archive(in root: URL, documentID: UUID) -> URL {
        root.appending(path: archiveName, directoryHint: .isDirectory)
            .appending(path: documentID.uuidString.lowercased(), directoryHint: .isDirectory)
    }

    static func receipt(in root: URL, documentID: UUID) -> URL {
        root.appending(path: receivedName, directoryHint: .isDirectory)
            .appending(path: "\(documentID.uuidString.lowercased()).json")
    }

    private static func isInsideICloudDrive(_ url: URL) -> Bool {
        url.pathComponents.contains(iCloudDriveName)
    }
}

@MainActor
@Observable
final class ICloudFolder {
    private static let bookmarkKey = "scanshelf.icloudFolderBookmark"

    /// 고른 폴더(보안 범위가 걸린 URL). 앱이 켜져 있는 동안 접근을 열어 둔다
    private(set) var pickedFolder: URL?
    private(set) var failure: String?

    var scanshelfRoot: URL? {
        pickedFolder.flatMap(ICloudFolderLayout.scanshelfRoot(forPicked:))
    }

    var isChosen: Bool {
        // UserDefaults는 관찰되지 않는다. pickedFolder를 읽어 두어 고르거나 잊을 때 화면이 다시 그려지게 한다
        _ = pickedFolder
        return UserDefaults.standard.data(forKey: Self.bookmarkKey) != nil
    }

    init() {
        _ = try? resolve()
    }

    /// `.fileImporter`가 돌려준 폴더를 기억한다
    func choose(_ url: URL) throws {
        guard ICloudFolderLayout.scanshelfRoot(forPicked: url) != nil else { throw ICloudFolderError.notICloudFolder }
        guard url.startAccessingSecurityScopedResource() else { throw ICloudFolderError.accessDenied }
        do {
            let bookmark = try url.bookmarkData(options: [], includingResourceValuesForKeys: nil, relativeTo: nil)
            UserDefaults.standard.set(bookmark, forKey: Self.bookmarkKey)
            pickedFolder?.stopAccessingSecurityScopedResource()
            pickedFolder = url
            failure = nil
        } catch {
            url.stopAccessingSecurityScopedResource()
            throw error
        }
    }

    func forget() {
        pickedFolder?.stopAccessingSecurityScopedResource()
        pickedFolder = nil
        UserDefaults.standard.removeObject(forKey: Self.bookmarkKey)
    }

    @discardableResult
    func resolve() throws -> URL {
        if let pickedFolder, let root = ICloudFolderLayout.scanshelfRoot(forPicked: pickedFolder) { return root }
        guard let bookmark = UserDefaults.standard.data(forKey: Self.bookmarkKey) else { throw ICloudFolderError.notChosen }
        var stale = false
        let url = try URL(resolvingBookmarkData: bookmark, options: [], relativeTo: nil, bookmarkDataIsStale: &stale)
        guard url.startAccessingSecurityScopedResource() else {
            failure = ICloudFolderError.accessDenied.localizedDescription
            throw ICloudFolderError.accessDenied
        }
        // 폴더를 옮기거나 이름을 바꾸면 오래된 북마크가 된다. 새로 만들어 둔다
        if stale, let fresh = try? url.bookmarkData(options: [], includingResourceValuesForKeys: nil, relativeTo: nil) {
            UserDefaults.standard.set(fresh, forKey: Self.bookmarkKey)
        }
        pickedFolder = url
        guard let root = ICloudFolderLayout.scanshelfRoot(forPicked: url) else { throw ICloudFolderError.notICloudFolder }
        return root
    }
}
