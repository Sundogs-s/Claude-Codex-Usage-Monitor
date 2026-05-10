# Claude Codex Usage Monitor

[中文](#中文说明) | [English](#english)

## 中文说明

### 项目简介
`Claude Codex Usage Monitor` 是一个 Windows 桌面实时用量监控工具，用于展示 Claude Code 与 Codex 的使用率与重置倒计时。

### 主要特性
- 实时双通道监控：同时显示 Claude 与 Codex。
- 多展示模式：支持悬浮窗与任务栏嵌入（AppBar）。
- 托盘交互：左键显示/隐藏，右键打开完整菜单。
- 动态托盘图标：托盘图标直接显示双进度条状态。
- 自动刷新与手动刷新：支持 5 秒 / 1 分钟 / 5 分钟策略。
- 日志与稳定性：运行日志输出到 `_runtime_logs/usage-monitor.log`。

### 技术栈
- Rust 2021
- Windows API (`windows` crate)
- Tokio + Reqwest

### 运行环境
- Windows 10/11
- 已安装并可用的 Claude/Codex CLI 环境（用于采集数据）

### 本地构建与运行
```bash
cargo build --release
.\target\release\usage-monitor.exe
```

### 发布版本下载
可执行文件会随 GitHub Release 发布，文件名：`usage-monitor.exe`。

---

## English

### Overview
`Claude Codex Usage Monitor` is a Windows desktop utility that shows real-time usage status for Claude Code and Codex, including utilization and reset countdowns.

### Features
- Dual real-time monitoring for Claude and Codex.
- Multiple display modes: floating window and taskbar-embedded AppBar.
- Tray interaction: left click to show/hide, right click for full menu.
- Dynamic tray icon with two progress bars.
- Auto/manual refresh with 5s / 1m / 5m intervals.
- Runtime logging to `_runtime_logs/usage-monitor.log`.

### Tech Stack
- Rust 2021
- Windows API via `windows` crate
- Tokio + Reqwest

### Requirements
- Windows 10/11
- Working Claude/Codex CLI environment for data collection

### Build & Run
```bash
cargo build --release
.\target\release\usage-monitor.exe
```

### Release Download
The executable is distributed through GitHub Releases as `usage-monitor.exe`.
