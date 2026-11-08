import SwiftUI

// 보관 현황: 문서가 장소마다 몇 개 있는지와 보내는 방식 설명. 목록 오른쪽 위 버튼으로 연다.
// 예전 "모두 보내기" 버튼은 앱을 열 때와 촬영을 마칠 때 저절로 하는 일을 다시 하는 것이라 이 화면 안으로 옮겼다.
// @see docs/transfer.md, docs/ios.md

struct StorageOverviewView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var sending = false

    var body: some View {
        let counts = StorageCounts.of(app: app)
        Form {
            Section {
                CountRow(title: "Mac에 있음", systemImage: "desktopcomputer", count: counts.onMac, tone: .green)
                if counts.onMac > 0 {
                    Text("Wi-Fi로 \(counts.viaWiFi)개, iCloud로 \(counts.viaICloud)개 받음")
                        .font(.footnote).foregroundStyle(.secondary)
                }
                CountRow(title: "iCloud에 보관됨", systemImage: "checkmark.icloud", count: counts.archived, tone: .blue)
                CountRow(title: "iCloud에 올라감, Mac 대기", systemImage: "icloud", count: counts.inInbox, tone: .blue)
                CountRow(title: "올리는 중", systemImage: "icloud.and.arrow.up", count: counts.uploading, tone: .orange)
                CountRow(title: "이 iPhone에만", systemImage: "iphone", count: counts.onlyHere, tone: .orange)
            } header: {
                Text("문서 \(counts.total)개")
            }

            Section {
                Text("찍는 즉시 iCloud scanshelf 폴더에 복사합니다. 앱을 지워도 스캔이 남습니다.")
                Text("같은 Wi-Fi에 Mac이 있으면 Wi-Fi로 바로 보내고, 떨어져 있으면 Mac이 켜질 때 iCloud에서 가져갑니다.")
                Text("Mac은 받은 원본을 iCloud scanshelf/archive에 계속 보관합니다. iPhone 원본은 Mac과 iCloud 보관이 모두 확인된 뒤에만 설정한 기간이 지나면 지웁니다.")
            } header: {
                Text("어떻게 옮겨지나")
            }
            .font(.footnote)

            Section {
                Button {
                    sending = true
                    Task {
                        await app.sendPending()
                        sending = false
                    }
                } label: {
                    HStack {
                        Label("지금 Mac으로 보내기", systemImage: "paperplane")
                        if sending {
                            Spacer()
                            ProgressView()
                        }
                    }
                }
                .disabled(app.settings.pairedMac == nil || sending)
            } footer: {
                Text(app.settings.pairedMac == nil
                    ? "연결한 Mac이 없습니다. 설정에서 Mac과 연결하면 Wi-Fi로 보낼 수 있습니다."
                    : "Mac에 아직 없는 문서를 Wi-Fi로 보내고 밀린 iCloud 복사를 깨웁니다. 앱을 열 때와 촬영을 마칠 때 저절로 하므로 보통은 누를 일이 없습니다.")
            }
        }
        .navigationTitle("보관 현황")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("닫기") { dismiss() }
            }
        }
    }
}

private struct CountRow: View {
    let title: String
    let systemImage: String
    let count: Int
    let tone: Color

    var body: some View {
        HStack {
            Label(title, systemImage: systemImage).foregroundStyle(count > 0 ? tone : .secondary)
            Spacer()
            Text("\(count)개").monospacedDigit().foregroundStyle(count > 0 ? .primary : .secondary)
        }
    }
}

/// 장소별 문서 수. 한 문서가 Mac과 iCloud에 모두 있으면 양쪽에 센다
struct StorageCounts: Equatable {
    var total = 0
    var onMac = 0
    var viaWiFi = 0
    var viaICloud = 0
    var archived = 0
    var inInbox = 0
    var uploading = 0
    var onlyHere = 0

    @MainActor
    static func of(app: AppModel) -> StorageCounts {
        tally(app.documents.filter { !$0.pages.isEmpty }.map { (app.status(of: $0), app.cloudStatus(of: $0)) })
    }

    static func tally(_ statuses: [(SendStatus, CloudStatus)]) -> StorageCounts {
        var counts = StorageCounts()
        for (mac, cloud) in statuses {
            counts.total += 1
            var safe = cloud.isSafe
            if case let .sent(method, _) = mac {
                counts.onMac += 1
                if method == .wifi { counts.viaWiFi += 1 } else { counts.viaICloud += 1 }
                safe = true
            }
            switch cloud {
            case .archived: counts.archived += 1
            case .inInbox: counts.inInbox += 1
            case .uploading: counts.uploading += 1
            case .off: break
            }
            if !safe { counts.onlyHere += 1 }
        }
        return counts
    }
}
