import AppKit
import SwiftUI
import Combine

enum CW {
    static let text = Color(red: 244/255, green: 239/255, blue: 233/255)
    static let muted = Color(red: 185/255, green: 174/255, blue: 164/255)
    static let red = Color(red: 212/255, green: 18/255, blue: 31/255)
    static let bright = Color(red: 1, green: 55/255, blue: 72/255)
    static let warning = Color(red: 217/255, green: 161/255, blue: 59/255)
    static let ok = Color(red: 70/255, green: 197/255, blue: 118/255)
    static let line = Color.white.opacity(0.09)
    static let images: [String: NSImage] = {
        var result: [String: NSImage] = [:]
        for name in ["claude", "codex", "cursor", "settings", "repeat", "external-link", "close", "desktop", "log-out"] {
            if let url = Bundle.main.url(forResource: name, withExtension: "png", subdirectory: "Icons"),
               let image = NSImage(contentsOf: url) {
                image.isTemplate = true
                result[name] = image
            }
        }
        return result
    }()
    static func color(_ used: Double) -> Color { used >= 1 ? bright : used >= 0.8 ? warning : text.opacity(0.75) }
    static func label(_ raw: String) -> String {
        switch raw { case "5h": return "5 小时"; case "7d": return "7 天"; case "Extra": return "额外用量"; default: return raw }
    }
    static func message(_ raw: String) -> String {
        if raw.contains("Starting collector") { return "正在连接本地采集器…" }
        if raw.contains("Bundled collector") { return "本地采集组件缺失，请重新安装。" }
        if raw.contains("Collector stopped") { return "采集器已断开，正在尝试重连。" }
        if raw.contains("Collector unavailable") { return "采集器不可用，请检查安装后重新连接。" }
        if raw.contains("Allow Usage Monitor") { return "请在系统设置中允许 Usage Monitor 登录项。" }
        if raw.contains("Cannot change login") { return "无法修改登录项，请将应用放入 Applications 并检查系统设置。" }
        if raw.contains("Cookie") { return "Cursor Cookie 缺失或已过期，请在设置中更新。" }
        if raw.contains("Login") { return "登录状态不可用，请通过对应 CLI 重新登录。" }
        if raw.contains("Rate limited") { return "平台限流，稍后自动重试。" }
        if raw.contains("Cannot refresh") { return "刷新失败，保留上次数据等待重试。" }
        return raw
    }
    static func countdown(_ reset: Int64) -> String {
        let seconds = max(0, reset - Int64(Date().timeIntervalSince1970))
        if seconds == 0 { return "待重置" }
        if seconds >= 86400 { return "\(seconds/86400)天\((seconds%86400)/3600)小时后重置" }
        if seconds >= 3600 { return "\(seconds/3600)小时\((seconds%3600)/60)分后重置" }
        return "\(max(1, seconds/60))分后重置"
    }
}

struct CWIcon: View {
    let name: String
    var size: CGFloat = 16
    var body: some View {
        Group {
            if let image = CW.images[name] { Image(nsImage: image).resizable().renderingMode(.template) }
            else { Image(systemName: "chart.bar").resizable() }
        }.scaledToFit().frame(width: size, height: size).accessibilityHidden(true)
    }
}

struct CWButtonStyle: ButtonStyle {
    var primary = false
    @Environment(\.isEnabled) private var enabled
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.font(.system(size: 12, weight: .semibold))
            .padding(.horizontal, 12).frame(minHeight: 30)
            .background(primary ? CW.red.opacity(configuration.isPressed ? 0.65 : 0.9) : Color.white.opacity(configuration.isPressed ? 0.1 : 0.04), in: Capsule())
            .overlay(Capsule().strokeBorder(CW.line, lineWidth: 1))
            .contentShape(Capsule()).opacity(!enabled ? 0.45 : configuration.isPressed ? 0.8 : 1)
    }
}

struct CWToggleStyle: ToggleStyle {
    func makeBody(configuration: Configuration) -> some View {
        Button { configuration.isOn.toggle() } label: {
            HStack(spacing: 8) {
                configuration.label.font(.system(size: 12))
                Spacer(minLength: 8)
                Capsule().fill(configuration.isOn ? CW.red : Color.white.opacity(0.12))
                    .frame(width: 30, height: 18)
                    .overlay(Circle().fill(CW.text).frame(width: 12, height: 12)
                        .offset(x: configuration.isOn ? 6 : -6))
            }.frame(minHeight: 30).contentShape(Rectangle())
        }.buttonStyle(.plain)
            .accessibilityElement(children: .combine).accessibilityValue(configuration.isOn ? "开启" : "关闭")
            .accessibilityAddTraits(.isButton)
    }
}

struct CWCard: ViewModifier {
    func body(content: Content) -> some View {
        content.padding(14).background(LinearGradient(colors: [Color.white.opacity(0.04), Color.white.opacity(0.018)], startPoint: .top, endPoint: .bottom), in: RoundedRectangle(cornerRadius: 18))
            .overlay(RoundedRectangle(cornerRadius: 18).strokeBorder(CW.line, lineWidth: 1))
    }
}

@MainActor
struct PanelGeometry {
    var width: CGFloat = 640
    var height: CGFloat = 590
    var metricColumns = 1
    var topHeight: CGFloat = 330
    var cursorHeight: CGFloat = 106

    static func fit(_ snapshot: Snapshot?, maximum: NSSize) -> PanelGeometry {
        let services = snapshot?.services.filter(\.enabled) ?? []
        let top = services.filter { $0.id != "cursor" }
        let hasCursor = services.contains { $0.id == "cursor" }
        let maxHeight = max(300, maximum.height - 20)
        func required(_ columns: Int, width: CGFloat) -> CGFloat {
            let cardWidth = top.count == 2 ? (width - 52)/2 : width - 40
            return top.map { service -> CGFloat in
                let host = NSHostingView(rootView: ServiceCard(service: service, disconnected: false, columns: columns)
                    .frame(width: cardWidth).preferredColorScheme(.dark))
                return ceil(host.fittingSize.height) + 2
            }.max() ?? 120
        }
        var columns = 1
        var width = min(maximum.width - 20, CGFloat(640))
        var cursorHeight: CGFloat = 0
        func cursorSize(_ width: CGFloat) -> CGFloat {
            guard let cursor = services.first(where: { $0.id == "cursor" }) else { return 0 }
            let host = NSHostingView(rootView: ServiceCard(service: cursor, disconnected: false, cursor: true)
                .frame(width: width - 40).preferredColorScheme(.dark))
            return ceil(host.fittingSize.height) + 2
        }
        cursorHeight = cursorSize(width)
        var overhead: CGFloat = 142 + (hasCursor ? cursorHeight + 12 : 0)
        while required(columns, width: width) + overhead > maxHeight && columns < 8 {
            columns += 1
            width = min(maximum.width - 20, max(640, CGFloat(columns) * 560))
            cursorHeight = cursorSize(width)
            overhead = 142 + (hasCursor ? cursorHeight + 12 : 0)
        }
        let topHeight = max(120, required(columns, width: width))
        let height = min(maxHeight, max(590, topHeight + overhead))
        return PanelGeometry(width: max(320, width), height: height, metricColumns: columns,
                             topHeight: max(120, height - overhead), cursorHeight: cursorHeight)
    }
}

struct UsageMetricView: View {
    let row: UsageRow
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(CW.label(row.label)).font(.system(size: 11)).foregroundStyle(CW.muted)
                Spacer(minLength: 2)
                Text(row.value ?? row.utilization.map { "\(Int(($0 * 100).rounded()))%" } ?? "—")
                    .font(.system(size: 13, weight: .bold, design: .monospaced))
                    .foregroundStyle(row.utilization.map(CW.color) ?? CW.text)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if row.value == nil {
                GeometryReader { proxy in
                    ZStack(alignment: .leading) {
                        Capsule().fill(Color.white.opacity(0.09))
                        if let used = row.utilization, used.isFinite {
                            Capsule().fill(CW.color(used)).frame(width: proxy.size.width * min(1, max(0, used)))
                        } else { Capsule().strokeBorder(CW.muted.opacity(0.4), style: StrokeStyle(lineWidth: 1, dash: [3,3])) }
                    }
                }.frame(height: 5)
                HStack {
                    Text(row.utilization == nil ? "暂无额度数据" : "已使用")
                    Spacer(minLength: 2)
                    if let reset = row.reset {
                        Text(CW.countdown(reset))
                    }
                }.font(.system(size: 10)).foregroundStyle(CW.muted.opacity(0.75))
                    .lineLimit(1).minimumScaleFactor(0.85)
            }
        }.frame(minHeight: row.value == nil ? 42 : 26, alignment: .top)
            .accessibilityElement(children: .combine)
    }
}

struct ServiceCard: View {
    let service: Service
    let disconnected: Bool
    var columns = 1
    var cursor = false
    private var outdated: Bool { service.stale || disconnected }
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 9) {
                CWIcon(name: service.id, size: 24)
                VStack(alignment: .leading, spacing: 1) {
                    Text(service.title).font(.system(size: 14, weight: .bold))
                    if !service.meta.isEmpty { Text(service.meta).font(.system(size: 10)).foregroundStyle(CW.muted) }
                }
                Spacer(minLength: 4)
                Circle().fill(outdated ? CW.warning : service.updated == nil && service.rows.allSatisfy { $0.utilization == nil } ? CW.muted : CW.ok).frame(width: 6, height: 6)
                Text(outdated ? "数据陈旧" : "已连接").font(.system(size: 10)).foregroundStyle(outdated ? CW.warning : CW.muted)
            }
            if let error = service.error {
                Text(CW.message(error)).font(.system(size: 11)).foregroundStyle(CW.warning).fixedSize(horizontal: false, vertical: true)
            }
            if service.rows.isEmpty { Text("等待额度数据…").font(.system(size: 12)).foregroundStyle(CW.muted) }
            LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 14, alignment: .top), count: cursor ? 2 : columns), alignment: .leading, spacing: 4) {
                ForEach(Array(service.rows.enumerated()), id: \.offset) { _, row in UsageMetricView(row: row) }
            }
            if let activity = service.activity, !activity.isEmpty {
                Rectangle().fill(CW.line).frame(height: 1)
                HStack { Text("Tokens 用量"); Spacer(); Text("服务端日期").foregroundStyle(CW.muted) }.font(.system(size: 10))
                LazyVGrid(columns: [GridItem(.flexible()),GridItem(.flexible())], spacing: 6) {
                    ForEach(Array(activity.enumerated()), id: \.offset) { _, metric in
                        VStack(alignment: .leading, spacing: 3) {
                            Text(metric.label).font(.system(size: 10)).foregroundStyle(CW.muted)
                            Text(metric.value ?? "—").font(.system(size: 21, weight: .bold, design: .rounded)).italic().monospacedDigit()
                        }.frame(maxWidth: .infinity, alignment: .leading).padding(8)
                            .background(Color.white.opacity(0.025), in: RoundedRectangle(cornerRadius: 12))
                    }
                }
            }
            if service.id == "claude", let detail = service.detail, !detail.isEmpty {
                Text(detail.replacingOccurrences(of: "Next retry:", with: "下次重试：")).font(.system(size: 10)).foregroundStyle(CW.muted)
            }
            if !cursor { Spacer(minLength: 0) }
        }.frame(maxWidth: .infinity, alignment: .topLeading).modifier(CWCard())
            .opacity(outdated ? 0.72 : 1)
    }
}

struct SettingsGroup<Content: View>: View {
    let title: String
    @ViewBuilder var content: Content
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.system(size: 12, weight: .bold)).foregroundStyle(CW.muted)
            content
            Spacer(minLength: 0)
        }.frame(maxWidth: .infinity, alignment: .topLeading).modifier(CWCard())
    }
}

@MainActor
struct MonitorView: View {
    @ObservedObject var model: Collector
    var floating = false
    var maximum = NSSize(width: 1400, height: 850)
    @AppStorage("UsageMonitorMiniBars") private var miniBars = true
    private var geometry: PanelGeometry { PanelGeometry.fit(model.snapshot, maximum: maximum) }
    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 10) {
                CWIcon(name: model.showingSettings ? "settings" : "desktop", size: 22).foregroundStyle(Color(red: 1, green: 179/255, blue: 186/255))
                    .frame(width: 34, height: 34).background(CW.red.opacity(0.14), in: RoundedRectangle(cornerRadius: 12))
                VStack(alignment: .leading, spacing: 2) {
                    Text(model.showingSettings ? "设置" : "用量总览").font(.system(size: 18, weight: .bold))
                    Text(!model.status.isEmpty ? CW.message(model.status) : !model.connected ? "正在连接采集器 · 保留最近数据" : model.showingSettings ? "显示、采集与更新" : "Claude · Codex · Cursor")
                        .font(.system(size: 11)).foregroundStyle(CW.muted).lineLimit(1).help(CW.message(model.status))
                }
                Spacer()
                if model.showingSettings {
                    Button("返回总览") { model.showingSettings = false }.keyboardShortcut(.cancelAction)
                } else {
                    Button { model.connected ? model.send(["action": "refresh"]) : model.retry() } label: {
                        HStack { CWIcon(name: "repeat", size: 14); Text(model.connected ? "刷新" : "重新连接") }
                    }
                }
                if floating { Button { model.closeFloating() } label: { CWIcon(name: "close", size: 14) }.help("关闭悬浮窗") }
            }.frame(height: 44)
            if model.showingSettings { settings }
            else { overview }
            HStack(spacing: 8) {
                Button { model.showingSettings.toggle() } label: { HStack { CWIcon(name: "settings", size: 14); Text(model.showingSettings ? "用量总览" : "设置") } }
                if model.showingSettings { UpdateCheckButton() }
                else if !floating {
                    Button { model.showFloating() } label: { HStack { CWIcon(name: "external-link", size: 14); Text("悬浮窗") } }
                }
                if !model.showingSettings { Button("退出") { NSApp.terminate(nil) } }
                Spacer(minLength: 0)
                Text("v\(model.snapshot?.version ?? Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "—") · Apple Silicon")
                    .font(.system(size: 10)).foregroundStyle(CW.muted.opacity(0.65))
            }.frame(height: 30)
        }.padding(20).frame(width: geometry.width, height: geometry.height)
            .foregroundStyle(CW.text).buttonStyle(CWButtonStyle()).toggleStyle(CWToggleStyle())
            .background {
                ZStack(alignment: .topLeading) {
                    Color(red: 7/255, green: 7/255, blue: 7/255)
                    LinearGradient(colors: [Color.white.opacity(0.025), .clear], startPoint: .top, endPoint: .bottom)
                    RadialGradient(colors: [CW.red.opacity(0.11), .clear], center: .topLeading, startRadius: 0, endRadius: 240)
                    Canvas { context, size in
                        var path = Path()
                        for x in stride(from: CGFloat(0), through: size.width, by: 34) { path.move(to: CGPoint(x:x,y:0)); path.addLine(to: CGPoint(x:x,y:size.height)) }
                        for y in stride(from: CGFloat(0), through: size.height, by: 34) { path.move(to: CGPoint(x:0,y:y)); path.addLine(to: CGPoint(x:size.width,y:y)) }
                        context.stroke(path, with: .color(.white.opacity(0.018)), lineWidth: 1)
                    }.mask(LinearGradient(colors: [.black,.clear], startPoint: .top, endPoint: .bottom))
                }
            }.clipShape(RoundedRectangle(cornerRadius: 22)).preferredColorScheme(.dark)
    }

    private var overview: some View {
        let enabled = model.snapshot?.services.filter(\.enabled) ?? []
        let top = enabled.filter { $0.id != "cursor" }
        return VStack(spacing: 12) {
            if !top.isEmpty {
                HStack(alignment: .top, spacing: 12) {
                    ForEach(top) { service in ServiceCard(service: service, disconnected: !model.connected, columns: geometry.metricColumns)
                            .frame(maxWidth: .infinity).frame(height: geometry.topHeight) }
                }
            }
            if let cursor = enabled.first(where: { $0.id == "cursor" }) {
                ServiceCard(service: cursor, disconnected: !model.connected, cursor: true)
                    .frame(height: geometry.cursorHeight)
            }
            if enabled.isEmpty { Text(model.snapshot == nil ? "等待用量数据…" : "尚未启用平台，请在设置中开启。")
                    .foregroundStyle(CW.muted).frame(maxWidth: .infinity, maxHeight: .infinity) }
        }
    }

    private var settings: some View {
        LazyVGrid(columns: [GridItem(.flexible(), alignment: .top), GridItem(.flexible(), alignment: .top)], alignment: .leading, spacing: 12) {
            SettingsGroup(title: "平台与菜单栏") {
                ForEach(model.snapshot?.services ?? []) { service in
                    Toggle(isOn: Binding(get: { model.snapshot?.services.first { $0.id == service.id }?.enabled ?? service.enabled },
                        set: { model.send(["action": "set_service", "service": service.id, "enabled": $0]) })) {
                        HStack { CWIcon(name: service.id); Text(service.title) }
                    }.disabled(!model.connected)
                }
                Toggle("缩略进度条", isOn: $miniBars)
                Text("菜单栏显示各平台最高的已用额度。")
                    .font(.system(size: 10)).foregroundStyle(CW.muted)
            }.frame(height: 216)
            SettingsGroup(title: "刷新与启动") {
                Text("刷新间隔").font(.system(size: 12))
                HStack {
                    ForEach([UInt64(60), UInt64(300)], id: \.self) { seconds in
                        Button(seconds == 60 ? "1 分钟" : "5 分钟") { model.send(["action": "set_refresh", "seconds": seconds]) }
                            .buttonStyle(CWButtonStyle(primary: model.snapshot?.refresh_seconds == seconds)).disabled(!model.connected)
                    }
                }
                Toggle("登录时启动", isOn: Binding(get: { model.launchAtLogin }, set: { model.setLogin($0) }))
                Text("实际刷新遵守平台限流与退避。")
                    .font(.system(size: 10)).foregroundStyle(CW.muted)
            }.frame(height: 216)
            SettingsGroup(title: "Cursor 认证") {
                HStack {
                    Text(model.snapshot?.cookie_configured == true ? "Cookie 已配置" : "未配置 Cookie").font(.system(size: 11)).foregroundStyle(CW.muted)
                    Spacer()
                    Button("清除") { model.send(["action": "clear_cookie"]) }.disabled(!model.connected)
                }
                SecureField("粘贴新的 Cursor Cookie", text: $model.cookieDraft).textFieldStyle(.plain)
                    .padding(9).background(Color.black.opacity(0.2), in: RoundedRectangle(cornerRadius: 14))
                    .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(CW.line)).disabled(!model.connected)
                Button("保存 Cookie") { model.saveCookie() }.disabled(!model.connected || model.cookieDraft.isEmpty)
                Text("Claude / Codex 使用本机 CLI 登录状态。")
                    .font(.system(size: 10)).foregroundStyle(CW.muted)
            }.frame(height: 184)
            SettingsGroup(title: "应用更新") { UpdateControls() }.frame(height: 184)
        }.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
    }
}

@MainActor
final class MonitorPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }
}

@MainActor
final class StatusController: NSObject {
    private let model: Collector
    private let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
    private let popover = NSPopover()
    private var subscription: AnyCancellable?
    private var defaultsObserver: NSObjectProtocol?
    private var renderedMiniBars = true
    init(model: Collector) {
        self.model = model
        super.init()
        item.button?.target = self
        item.button?.action = #selector(toggle)
        popover.behavior = .transient
        popover.animates = false
        subscription = model.$snapshot.combineLatest(model.$connected, model.$showingSettings)
            .receive(on: RunLoop.main).sink { [weak self] snapshot, connected, _ in
                self?.render(snapshot, connected: connected)
            }
        defaultsObserver = NotificationCenter.default.addObserver(forName: UserDefaults.didChangeNotification, object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor [weak self] in
                guard let self = self else { return }
                let bars = UserDefaults.standard.object(forKey: "UsageMonitorMiniBars") as? Bool ?? true
                if bars != self.renderedMiniBars { self.render(self.model.snapshot, connected: self.model.connected) }
            }
        }
        render(model.snapshot, connected: model.connected)
    }
    private func render(_ snapshot: Snapshot?, connected: Bool) {
        let services = snapshot?.services.filter(\.enabled) ?? []
        let bars = UserDefaults.standard.object(forKey: "UsageMonitorMiniBars") as? Bool ?? true
        renderedMiniBars = bars
        let width: CGFloat = services.isEmpty ? 22 : CGFloat(services.count) * 66 + 4
        let image = NSImage(size: NSSize(width: width, height: 20), flipped: false) { rect in
            NSColor.black.setFill()
            if services.isEmpty { CW.images["desktop"]?.draw(in: NSRect(x: 3, y: 2, width: 16, height: 16)); return true }
            for (index, service) in services.enumerated() {
                let x = CGFloat(index) * 66 + 4
                CW.images[service.id]?.draw(in: NSRect(x: x, y: 2, width: 15, height: 15))
                let quota = service.rows.compactMap(\.utilization).filter(\.isFinite).max()
                let value = quota.map { "\(Int(($0 * 100).rounded()))%" } ?? "—"
                let attrs: [NSAttributedString.Key: Any] = [.font: NSFont.monospacedDigitSystemFont(ofSize: 10, weight: .semibold), .foregroundColor: NSColor.black]
                let text = NSAttributedString(string: value, attributes: attrs)
                text.draw(at: NSPoint(x: x + 20 + max(0, (34 - text.size().width) / 2), y: bars ? 8 : 4))
                if bars {
                    let track = NSRect(x: x+20, y: 3, width: 34, height: 3)
                    NSColor.black.withAlphaComponent(0.2).setFill()
                    NSBezierPath(roundedRect: track, xRadius: 1.5, yRadius: 1.5).fill()
                    if let quota = quota {
                        NSColor.black.setFill()
                        NSBezierPath(roundedRect: NSRect(x: track.minX, y: track.minY, width: 34 * min(1,max(0,quota)), height: 3), xRadius: 1.5, yRadius: 1.5).fill()
                    }
                }
                if service.stale || !connected { NSColor.black.setFill(); NSBezierPath(ovalIn: NSRect(x: x+57, y: 13, width: 3, height: 3)).fill() }
            }
            return true
        }
        image.isTemplate = true
        item.button?.image = image
        item.button?.toolTip = services.map { service in
            let row = service.rows.filter { $0.utilization?.isFinite == true }.max { ($0.utilization ?? 0) < ($1.utilization ?? 0) }
            return "\(service.title) · \(row.map { CW.label($0.label) } ?? "暂无额度") · \(row?.utilization.map { "\(Int(($0 * 100).rounded()))% 已使用" } ?? "—")\(service.stale || !connected ? " · 数据陈旧" : "")"
        }.joined(separator: "\n")
        let screen = item.button?.window?.screen ?? NSScreen.main
        let maximum = screen?.visibleFrame.size ?? NSSize(width: 1400, height: 850)
        let geometry = PanelGeometry.fit(snapshot, maximum: maximum)
        let controller = (popover.contentViewController as? NSHostingController<MonitorView>) ?? NSHostingController(rootView: MonitorView(model: model, maximum: maximum))
        controller.rootView = MonitorView(model: model, maximum: maximum)
        popover.contentViewController = controller
        popover.contentSize = NSSize(width: geometry.width, height: geometry.height)
        model.resizeFloating()
    }
    @objc private func toggle() {
        guard let button = item.button else { return }
        if popover.isShown { popover.performClose(nil) }
        else {
            model.showingSettings = false
            render(model.snapshot, connected: model.connected)
            popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
            popover.contentViewController?.view.window?.makeKey()
            NSApp.activate(ignoringOtherApps: true)
        }
    }
    func writePreview(_ destination: URL) throws {
        guard CommandLine.arguments.contains("--ui-self-test") else { return }
        render(model.snapshot, connected: model.connected)
        guard let image = item.button?.image, let tiff=image.tiffRepresentation,
              let bitmap=NSBitmapImageRep(data:tiff), let data=bitmap.representation(using:.png,properties:[:]),
              image.size.height<=22, image.size.width>150 else { throw NSError(domain:"StatusUI",code:1) }
        try data.write(to:destination)
    }
    deinit { if let observer = defaultsObserver { NotificationCenter.default.removeObserver(observer) } }
}
