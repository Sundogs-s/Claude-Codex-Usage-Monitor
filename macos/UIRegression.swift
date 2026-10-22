import AppKit
import SwiftUI

extension Snapshot {
    static func uiFixture(rows: Int = 5, plus: Bool = false, unknown: Bool = false) -> Snapshot {
        let now = Int64(Date().timeIntervalSince1970)
        let claude = Service(id: "claude", title: "Claude", enabled: true, meta: "Max · 20×", stale: false, error: nil, updated: now,
            rows: (0..<rows).map { index in
                UsageRow(label: index<5 ? ["5h","7d","Sonnet","Opus","Extra"][index] : "Model \(index)", utilization: unknown ? nil : [0.32,0.48,0.24,0.61,0.0][index%5], reset: unknown ? nil : now+86400, value: nil)
            }, detail: nil, activity: [])
        var codexRows=[UsageRow(label: "7d", utilization: unknown ? nil : 0.64, reset: unknown ? nil : now+302400, value: nil)]
        if plus { codexRows.insert(UsageRow(label:"5h",utilization:0.42,reset:now+7200,value:nil),at:0); codexRows.append(UsageRow(label:"Credits",utilization:nil,reset:nil,value:"100.00")); codexRows.append(UsageRow(label:"可重置次数",utilization:nil,reset:nil,value:"3")) }
        let codex = Service(id: "codex", title: "Codex", enabled: true, meta: plus ? "Plus" : "Pro", stale: false, error: nil, updated: now,
            rows: codexRows, detail: nil,
            activity: [ActivityMetric(label: "今日", value: "1.28M"), ActivityMetric(label: "昨日", value: "946K"), ActivityMetric(label: "近 7 天", value: "8.42M")])
        let cursor = Service(id: "cursor", title: "Cursor", enabled: true, meta: "", stale: false, error: nil, updated: nil,
            rows: [UsageRow(label: "AUTO", utilization: 0.18, reset: now+864000, value: nil),UsageRow(label: "API", utilization: 0.86, reset: now+864000, value: nil)], detail: nil, activity: [])
        return Snapshot(schema: 1, version: "UI fixture", services: [claude,codex,cursor], refresh_seconds: 60, cookie_configured: true, message: nil)
    }
}

@MainActor
func runUIRegression(status: StatusController?) {
    guard let index = CommandLine.arguments.firstIndex(of: "--ui-self-test"), CommandLine.arguments.count > index+1 else { return }
    let destination = URL(fileURLWithPath: CommandLine.arguments[index+1], isDirectory: true)
    do {
        try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
        guard CW.images.count == 9 else { throw NSError(domain: "UI", code: 1, userInfo: [NSLocalizedDescriptionKey:"Missing bundled brand/action images"]) }
        let model=Collector.shared
        try status?.writePreview(destination.appendingPathComponent("macos-menubar.png"))
        for (name,setting,fixture) in [("overview",false,Snapshot.uiFixture()),("settings",true,Snapshot.uiFixture()),("plus",false,Snapshot.uiFixture(plus:true)),("dynamic",false,Snapshot.uiFixture(rows:17)),("unknown",false,Snapshot.uiFixture(unknown:true))] {
            model.applyUIFixture(fixture)
            model.showingSettings=setting
            let geometry=PanelGeometry.fit(model.snapshot,maximum:NSSize(width:1400,height:850))
            let top=fixture.services.filter { $0.enabled && $0.id != "cursor" }
            for service in top {
                let width=top.count==2 ? (geometry.width-52)/2 : geometry.width-40
                let natural=NSHostingView(rootView:ServiceCard(service:service,disconnected:false,columns:geometry.metricColumns).frame(width:width))
                guard natural.fittingSize.height<=geometry.topHeight else { throw NSError(domain:"UI",code:5,userInfo:[NSLocalizedDescriptionKey:"Service card content exceeds allocated height"]) }
            }
            let host=NSHostingView(rootView:MonitorView(model:model,maximum:NSSize(width:1400,height:850)))
            host.frame=NSRect(x:0,y:0,width:geometry.width,height:geometry.height)
            let window=MonitorPanel(contentRect:host.frame,styleMask:[.borderless],backing:.buffered,defer:false)
            window.contentView=host
            host.layoutSubtreeIfNeeded()
            guard let bitmap=host.bitmapImageRepForCachingDisplay(in:host.bounds) else { throw NSError(domain:"UI",code:2) }
            host.cacheDisplay(in:host.bounds,to:bitmap)
            guard let png=bitmap.representation(using:.png,properties:[:]) else { throw NSError(domain:"UI",code:3) }
            try png.write(to:destination.appendingPathComponent("macos-\(name).png"))
            guard host.fittingSize.width <= geometry.width+1, host.fittingSize.height <= geometry.height+1 else { throw NSError(domain:"UI",code:4,userInfo:[NSLocalizedDescriptionKey:"Hosting view exceeds panel bounds"]) }
        }
        print("PASS: native hosting/rendering, overview/settings bounds, all bundled template icons; fixture only, no collection")
        NSApp.terminate(nil)
    } catch {
        fputs("UI regression failed: \(error)\n",stderr)
        exit(1)
    }
}
