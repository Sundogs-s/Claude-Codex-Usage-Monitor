# Runtime Logging

## 日志文件路径
- 项目运行日志固定输出到：`_runtime_logs/usage-monitor.log`
- 绝对路径示例：`F:\Projects\Claude_UsageMointor\_runtime_logs\usage-monitor.log`

## 记录内容（重点）
- 启动信息：版本、日志路径、窗口初始样式与位置。
- AppBar 关键流程：
  - 注册前任务栏测量结果（任务栏高度、托盘左边界、目标宽度）。
  - `ABM_QUERYPOS / ABM_SETPOS` 后 Shell 返回的矩形。
  - `SetWindowPos` 后窗口状态（style/exstyle、window/client rect、visible）。
  - `WM_APPBAR` 回调触发时的重定位信息。
- AppBar 绘制状态（节流记录）：窗口尺寸、显示开关、核心利用率字段。

## 排查建议
- 每次复现后，直接提供该日志文件最新 200-400 行即可快速定位。
- 若需要更高频日志，可临时把 `overlay.rs` 内 AppBar 绘制日志节流阈值调低。
