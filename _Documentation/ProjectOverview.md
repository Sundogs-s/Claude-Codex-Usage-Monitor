# Project Overview

## 技术栈与形态
- 语言与构建：Rust 2021 + Cargo。
- 平台接口：Windows Win32 API（窗口、托盘、AppBar、GDI 绘制）。
- 网络与并发：tokio + reqwest（后台轮询 Claude/Codex 用量）。

## 当前代码结构
- `src/main.rs`：程序入口、窗口生命周期、托盘菜单、显示模式切换（Floating/CompactBar/AppBar）、自动启动注册表写入。
- `src/overlay.rs`：GDI 绘制层，负责悬浮窗/紧凑条/任务栏嵌入三种 UI 的渲染与托盘动态图标生成。
- `src/monitor.rs`：后台轮询线程与刷新调度（支持手动唤醒与刷新间隔切换）。
- `src/claude.rs`：Claude 认证读取、用量接口请求与回退逻辑。
- `src/codex.rs`：Codex 认证读取、WHAM 用量接口请求与兼容解析。
- `src/state.rs`：共享状态、设置持久化、显示配置与倒计时/百分比格式化。

## 运行与配置要点
- 设置文件路径：`%APPDATA%/UsageMonitor/settings.json`。
- Claude 凭据来源：用户目录下 `.claude` 相关凭据文件。
- Codex 凭据来源：用户目录下 `.codex/auth.json`。
- 日志默认输出：用户目录 `usage-monitor.log`。

## 现阶段重点（与 ProjectGuide 对齐）
- 继续修复初版问题，优先稳定性、数据正确性与 UI 模式切换一致性。
- 重点关注托盘/窗口状态同步、AppBar 场景兼容、认证失效后的恢复链路。