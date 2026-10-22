import SwiftUI
import Combine
import Sparkle

// Sparkle handles archive authentication, replacement, authorization and relaunch.
// A development build without feed/key exposes that state without a fallback source.
@MainActor
final class AppUpdater: NSObject, ObservableObject, SPUUpdaterDelegate {
    static let shared = AppUpdater()
    @Published var canCheck = false
    @Published var automaticChecks = false
    @Published var automaticDownloads = false
    @Published var preview = UserDefaults.standard.bool(forKey: "UsageMonitorUpdatePreview")
    private(set) var configured = false
    private var controller: SPUStandardUpdaterController?
    private var stableURL: URL?
    private var previewURL: URL?

    override private init() {
        super.init()
        if CommandLine.arguments.contains("--ui-self-test") { configured = true; automaticChecks = true; return }
        let info = Bundle.main.infoDictionary ?? [:]
        stableURL = (info["SUFeedURL"] as? String).flatMap(URL.init(string:))
        previewURL = (info["UsageMonitorPreviewFeedURL"] as? String).flatMap(URL.init(string:))
        guard Self.validFeedURL(stableURL), Self.validFeedURL(previewURL),
              let key = info["SUPublicEDKey"] as? String, Data(base64Encoded: key)?.count == 32 else { return }
        configured = true
        let instance = SPUStandardUpdaterController(startingUpdater: false, updaterDelegate: self, userDriverDelegate: nil)
        controller = instance
        instance.updater.publisher(for: \.canCheckForUpdates).assign(to: &$canCheck)
        instance.updater.publisher(for: \.automaticallyChecksForUpdates).assign(to: &$automaticChecks)
        instance.updater.publisher(for: \.automaticallyDownloadsUpdates).assign(to: &$automaticDownloads)
        instance.startUpdater()
    }

    private static func validFeedURL(_ url: URL?) -> Bool {
        guard let url = url else { return false }
        return url.scheme == "https" && url.host != nil && url.user == nil && url.password == nil
            && url.query == nil && url.fragment == nil
    }

    func check() { if canCheck { controller?.checkForUpdates(nil) } }
    func setChecks(_ enabled: Bool) { controller?.updater.automaticallyChecksForUpdates = enabled }
    func setDownloads(_ enabled: Bool) { controller?.updater.automaticallyDownloadsUpdates = enabled }
    func setPreview(_ enabled: Bool) {
        preview = enabled
        UserDefaults.standard.set(enabled, forKey: "UsageMonitorUpdatePreview")
        controller?.updater.resetUpdateCycle()
    }
    func feedURLString(for updater: SPUUpdater) -> String? {
        (preview ? previewURL : stableURL)?.absoluteString
    }
    func updaterWillRelaunchApplication(_ updater: SPUUpdater) { Collector.shared.stop() }
}

@MainActor
struct UpdateCheckButton: View {
    @ObservedObject private var updater = AppUpdater.shared
    var body: some View {
        Button { updater.check() } label: { HStack { CWIcon(name: "repeat", size: 14); Text("检查更新") } }
            .disabled(!updater.configured || !updater.canCheck)
    }
}

@MainActor
struct UpdateControls: View {
    @ObservedObject private var updater = AppUpdater.shared
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if updater.configured {
                Toggle("自动检查更新", isOn: Binding(get: { updater.automaticChecks }, set: updater.setChecks))
                Toggle("自动下载更新", isOn: Binding(get: { updater.automaticDownloads }, set: updater.setDownloads))
                Toggle("接收预览版", isOn: Binding(get: { updater.preview }, set: updater.setPreview))
            } else {
                Text("此构建未配置签名更新源。")
                    .font(.system(size: 11)).foregroundStyle(CW.muted)
            }
        }
    }
}
