# 界面资源

- 品牌图形：LobeHub `@lobehub/icons-static-svg` 的 Claude、Codex、Cursor；商标归各品牌所有。
- 操作图标：coolicons，Kryston Schwarze，CC BY 4.0，https://github.com/krystonschwarze/coolicons。
- `tokens.json` 来自千往 choseway-style 技能，2026-10-13 快照。
- SVG 保留原始轮廓，PNG 为 96×96 白色透明图形，mask 为相同尺寸的 8 位 alpha。Mac 使用模板 PNG，Windows 使用 alpha mask 按当前主题颜色绘制；无在线资源依赖。
- 生成：`node scripts/build-ui-assets.cjs <sharp 包绝对路径>`。sharp 仅用于开发期生成，不加入应用运行依赖。
