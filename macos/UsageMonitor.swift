import AppKit
import SwiftUI
import ServiceManagement

// macOS 13+, Apple Silicon. The bundled Rust child owns all provider I/O.
struct Snapshot: Decodable {
    let schema: Int
    let version: String
    let services: [Service]
    let refresh_seconds: UInt64
    let cookie_configured: Bool
    let message: String?
}
struct Service: Decodable, Identifiable {
    let id: String
    let title: String
    let enabled: Bool
    let meta: String
    let stale: Bool
    let error: String?
    let updated: Int64?
    let rows: [UsageRow]
    let detail: String?
    let activity: [ActivityMetric]?
}
struct ActivityMetric: Decodable {
    let label: String
    let value: String?
}
struct UsageRow: Decodable {
    let label: String
    let utilization: Double?
    let reset: Int64?
    let value: String?
}

// A serial decoder bounds partial frames and never touches UI state off the main actor.
final class FramePump: @unchecked Sendable {
    private let queue = DispatchQueue(label: "UsageMonitor.collector.frames")
    private var buffer = Data()
    private var failed = false
    private let deliver: (Snapshot?) -> Void
    init(deliver: @escaping (Snapshot?) -> Void) { self.deliver = deliver }
    func receive(_ bytes: Data) {
        queue.async { [self] in
            guard !failed else { return }
            guard !bytes.isEmpty else { failed = true; deliver(nil); return }
            buffer.append(bytes)
            while let newline = buffer.firstIndex(of: 10) {
                let frame = Data(buffer[..<newline])
                buffer.removeSubrange(...newline)
                guard frame.count <= 1_048_576,
                      let view = try? JSONDecoder().decode(Snapshot.self, from: frame), view.schema == 1 else {
                    failed = true; deliver(nil); return
                }
                deliver(view)
            }
            if buffer.count > 1_048_576 { failed = true; deliver(nil) }
        }
    }
}

@MainActor
final class Collector: ObservableObject {
    static let shared = Collector()
    @Published private(set) var snapshot: Snapshot?
    @Published private(set) var connected = false
    @Published private(set) var status = "Starting collector…"
    @Published var showingSettings = false
    @Published var cookieDraft = ""
    @Published var launchAtLogin = SMAppService.mainApp.status == .enabled
    private var child: Process?
    private var input: FileHandle?
    private var output: FileHandle?
    private var generation = UUID()
    private var attempts = 0
    private var stopping = false
    private var wakeObserver: NSObjectProtocol?
    private var panel: NSPanel?

    private init() {
        wakeObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didWakeNotification, object: nil, queue: .main
        ) { [weak self] _ in Task { @MainActor [weak self] in self?.send(["action": "refresh"]) } }
        start()
    }

    func applyUIFixture(_ fixture: Snapshot) {
        guard CommandLine.arguments.contains("--ui-self-test") else { return }
        snapshot = fixture
    }

    func retry() {
        guard child == nil else { return }
        attempts = 0
        start()
    }

    private func start() {
        if CommandLine.arguments.contains("--ui-self-test") { snapshot = .uiFixture(); connected = true; status = ""; return }
        guard !stopping, child == nil else { return }
        guard let url = Bundle.main.url(forResource: "usage-monitor-core", withExtension: nil),
              FileManager.default.isExecutableFile(atPath: url.path) else {
            status = "Bundled collector is missing. Rebuild the app."
            return
        }
        let process = Process()
        let stdinPipe = Pipe(), stdoutPipe = Pipe()
        let token = UUID()
        generation = token
        process.executableURL = url
        process.arguments = ["--stdio"]
        process.standardInput = stdinPipe
        process.standardOutput = stdoutPipe
        // Never forward provider stderr or credentials to Console / the menu.
        process.standardError = FileHandle.nullDevice
        process.terminationHandler = { [weak self] _ in
            Task { @MainActor [weak self] in self?.disconnected(token) }
        }
        let handle = stdoutPipe.fileHandleForReading
        let pump = FramePump { [weak self] view in
            Task { @MainActor [weak self] in
                guard let self = self, self.generation == token, !self.stopping else { return }
                guard let view = view else { self.disconnected(token); return }
                self.snapshot = view
                self.connected = true
                self.status = view.message ?? ""
            }
        }
        handle.readabilityHandler = { stream in
            let bytes = stream.availableData
            if bytes.isEmpty { stream.readabilityHandler = nil }
            pump.receive(bytes)
        }
        child = process
        input = stdinPipe.fileHandleForWriting
        output = handle
        do { try process.run() }
        catch { disconnected(token) }
    }

    private func disconnected(_ token: UUID) {
        guard generation == token else { return }
        generation = UUID() // EOF and termination callback must not restart twice.
        connected = false
        output?.readabilityHandler = nil
        try? output?.close()
        try? input?.close() // EOF asks the collector to cancel in-flight CLI children.
        output = nil
        input = nil
        let previous = child
        child = nil
        // Allow the Rust cancellation path time to reap its subprocesses.
        DispatchQueue.main.asyncAfter(deadline: .now() + 8) {
            if previous?.isRunning == true { previous?.terminate() }
        }
        guard !stopping else { return }
        status = "Collector stopped; last values may be outdated."
        guard attempts < 3 else {
            status = "Collector unavailable. Choose Retry after checking installation."
            return
        }
        attempts += 1
        let nextGeneration = generation
        DispatchQueue.main.asyncAfter(deadline: .now() + Double(8 + attempts * 2)) { [weak self] in
            guard let self = self, self.generation == nextGeneration, !self.stopping else { return }
            self.start()
        }
    }

    func send(_ command: [String: Any]) {
        guard let input = input, let bytes = try? JSONSerialization.data(withJSONObject: command) else { return }
        var frame = bytes
        frame.append(10)
        do { try input.write(contentsOf: frame) }
        catch { disconnected(generation) }
    }

    func saveCookie() {
        guard cookieDraft.utf8.count <= 32000, cookieDraft.contains("=") else { status = "Cookie 格式无效或过长，请检查后重试。"; return }
        status = ""
        send(["action": "set_cookie", "cookie": cookieDraft])
        cookieDraft = ""
    }

    func setLogin(_ enabled: Bool) {
        do {
            if enabled { try SMAppService.mainApp.register() }
            else { try SMAppService.mainApp.unregister() }
            launchAtLogin = SMAppService.mainApp.status == .enabled
            if SMAppService.mainApp.status == .requiresApproval {
                status = "Allow Usage Monitor in System Settings → General → Login Items."
                SMAppService.openSystemSettingsLoginItems()
            }
        } catch {
            launchAtLogin = SMAppService.mainApp.status == .enabled
            status = "Cannot change login item. Install the app in Applications and check Login Items."
        }
    }

    func showFloating() {
        if let panel = panel { panel.makeKeyAndOrderFront(nil); NSApp.activate(ignoringOtherApps: true); return }
        let maximum = (NSScreen.main?.visibleFrame.size ?? NSSize(width: 1400, height: 850))
        let geometry = PanelGeometry.fit(snapshot, maximum: maximum)
        let window = MonitorPanel(contentRect: NSRect(x: 0, y: 0, width: geometry.width, height: geometry.height),
            styleMask: [.borderless], backing: .buffered, defer: false)
        window.title = "Usage Monitor"
        window.level = .floating
        window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        window.isReleasedWhenClosed = false
        window.hidesOnDeactivate = false
        window.isMovableByWindowBackground = true
        window.isOpaque = false
        window.backgroundColor = .clear
        window.hasShadow = true
        window.contentView = NSHostingView(rootView: MonitorView(model: self, floating: true, maximum: maximum))
        window.setFrameAutosaveName("UsageMonitorFloating")
        if !window.setFrameUsingName("UsageMonitorFloating") { window.center() }
        panel = window
        resizeFloating()
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    func closeFloating() { panel?.orderOut(nil) }

    func resizeFloating() {
        guard let window = panel else { return }
        let screen = window.screen ?? NSScreen.main
        let maximum = screen?.visibleFrame.size ?? NSSize(width: 1400, height: 850)
        let geometry = PanelGeometry.fit(snapshot, maximum: maximum)
        if let host = window.contentView as? NSHostingView<MonitorView> {
            host.rootView = MonitorView(model: self, floating: true, maximum: maximum)
        } else { window.contentView = NSHostingView(rootView: MonitorView(model: self, floating: true, maximum: maximum)) }
        var frame = NSRect(x: window.frame.minX, y: window.frame.maxY - geometry.height,
                           width: geometry.width, height: geometry.height)
        if let bounds = screen?.visibleFrame {
            frame.origin.x = min(max(frame.minX, bounds.minX), bounds.maxX - frame.width)
            frame.origin.y = min(max(frame.minY, bounds.minY), bounds.maxY - frame.height)
        }
        if window.frame != frame { window.setFrame(frame, display: true) }
    }

    func stop() {
        guard !stopping else { return }
        stopping = true
        if let observer = wakeObserver { NSWorkspace.shared.notificationCenter.removeObserver(observer) }
        send(["action": "quit"])
        try? input?.close()
        input = nil
        output?.readabilityHandler = nil
        // Bounded wait gives the collector a chance to kill/reap its own CLI child.
        let deadline = Date().addingTimeInterval(8)
        while child?.isRunning == true && Date() < deadline { Thread.sleep(forTimeInterval: 0.025) }
        if child?.isRunning == true { child?.terminate() }
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var statusController: StatusController?
    func applicationDidFinishLaunching(_ notification: Notification) {
        statusController = StatusController(model: Collector.shared)
        if CommandLine.arguments.contains("--ui-self-test") { DispatchQueue.main.asyncAfter(deadline: .now()+0.3) { runUIRegression(status: self.statusController) } }
    }
    func applicationWillTerminate(_ notification: Notification) { Collector.shared.stop() }
}

@main
@MainActor
struct UsageMonitorApp: App {
    private let updater = AppUpdater.shared
    @NSApplicationDelegateAdaptor(AppDelegate.self) var delegate
    var body: some Scene { Settings { EmptyView() } }
}
