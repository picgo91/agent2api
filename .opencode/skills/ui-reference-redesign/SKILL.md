---
name: ui-reference-redesign
description: Use when restyling this desktop app's UI to match a reference project's look, or when CSS/UI changes appear to have no visual effect. Covers locating the stylesheet layer that actually renders, porting design tokens by role, previewing for approval before committing, and verifying Tailwind v4 output. 触发词：UI 重做、改样式、参考项目、换皮、design token、样式不生效、ui.css、theme.css、令牌、预览。
---

# 按参考项目重做 UI 样式

本仓库是 Tauri 桌面端 + Docker 镜像。UI 分两层：原生外壳（`desktop-tauri/ui/`）与 React 岛（`desktop-tauri/ui-islands/`）。这个流程是踩过坑之后定下来的，**顺序不要跳**——第 0 步跳了就白改好几轮。

## 0. 先确认哪一层真的在渲染

**这一步不做，后面全是白费力气。** 本项目有过连续几轮只改 `ui/css/*.css`、页面观感几乎没变的经历，根因是 `ui/islands/ui.css` 在 `index.html` 里**最后加载**，把原生层的 body 规则整个盖掉了。

开工前先做这三件事：

1. 读 `desktop-tauri/ui/index.html` 的 `<link>` / `<script>` 顺序，**确认最后加载的是谁**。最后那个赢整片区域。
2. grep 框架痕迹，别信默认假设。本项目**没有** `createTheme` / `ThemeProvider` / `CssBaseline`，也没有 MUI JS Theme；实际驱动页面正文的是 `ui-kit/styles/theme.css` 的 `--ui-*` 自定义属性 + Tailwind v4 工具类。改样式前先确认目标区域到底由谁控制。
3. 令牌分两套且**必须按角色对齐**：
   - `desktop-tauri/ui-kit/styles/theme.css` → `--ui-*`，喂 React / Tailwind
   - `desktop-tauri/ui/css/tokens.css` → 无前缀，喂原生外壳

## 1. 清点参考项目，别急着搬

对参考项目（例：`C:\Users\xcazt\Desktop\ymzf\workbuddy\php\templates.php` 的 `uiStyles()` / `layout()`）逐条抄下确切值：色值、圆角、投影、字重字距、内边距。**先问两个问题再动手**：

- **它真在用吗？** 参考项目自己会留死代码。实际发生过：`.ui-page-grad` 在 `templates.php` 里有定义，但 6 个 PHP 页面**零引用**——搬过来只是给自己加一份没人调用的样式。
- **目标项目是不是已经有了？** 也实际发生过：表头 `letter-spacing: .05em` 两张表（`.acct-table` / `.models-table`）本来就带，白查一遍。

清点结论要**明确告诉用户**哪些不搬、为什么。这比默默跳过有价值。

## 2. 令牌按角色映射，不要按 hex 搬

参考项目的色值直接抄过来会坏两件事：对比度、和既有语义色打架。

- **对比度要当场验算并说明**。本项目保留的三处刻意偏离，先例如下：
  - 第三档文字 `#94a3b8` → `#64748b`（前者对白底不足 AA）
  - 主色实底取渐变**深端** `#2563eb`（保证白字对比度），浅端 `#3b82f6` 另立 `--primary-lite` 给渐变起点
  - 参考项目没有完整暗色模式，暗色按其明度关系**反推**，并说明这是推断值
- **中性指标不许借语义色**。统计卡的六个图标块用装饰性色相轮换，不用 `--ok` / `--warn`——总 Token 这种中性读数没有好坏之分，借语义色会带上原本不存在的暗示。与该文件里「热力图用琥珀而不用 warn」是同一条理由。
- 新令牌要在 light / dark / system **三块都写**。`system` 块用 `sync-system-theme.mjs` 从 `dark` 重新生成，不要手抄。

## 3. 分轮改，每轮先预览再提交

**用户要求先看到效果再决定。** 每一轮：改 → 构建 → 起静态服务 → 让用户看 → 拿到明确答复 → 才提交。

```bash
npm --prefix desktop-tauri/ui-kit      run typecheck
npm --prefix desktop-tauri/ui-islands run typecheck
npm --prefix desktop-tauri/ui-islands run build     # 产出 ui/islands/ui.{css,js}，要一起提交
python -m http.server 8712 --directory desktop-tauri/ui
# 打开时带时间戳绕缓存：http://127.0.0.1:8712/index.html?v=<unix秒>
```

浏览器里**没有 Tauri bridge，`window.wb*` 全空 → 数据面板空白是预期**，不是 bug。

一轮的范围别贪大。令牌层一轮、组件层一轮、大改结构再一轮。每轮都能独立回滚。

## 组件层的具体坑

**别改被借用的通用类。** 报表页的概览格子原本借用 `.field-grid` / `.field`（全站表单通用类）。改这两个会连带改坏所有表单。正确做法是新立 `.stat-grid` 一族，只在那一个 `div` 上换类名。判断依据：`grep` 一下这个类被几个页面用了。

**新组件优先落在已有的中心 helper 上。** 空态有 `placeholder()`（7 处调用）而不是逐个页面改 markup。同理 `icons.js` 的 `wbIcons.icon(name, size)` 是全站唯一图标入口——复用它而不是新画 SVG，更不要用 emoji（`icons.js` 顶部写了理由：emoji 是字体字形，跨机器粗细/基线都不同，无法精确着色）。

**`:has()` 处理「两种形态共用一个类」。** `.empty` 既被纯文本空态用也被结构化空态用：基础留白给纯文本档，`.empty:has(.t)` 才放大留白给带图标+标题+说明的那档。这样不用给每个调用点传内联 padding。

**`overflow: clip` 会吃掉自己的投影。** `.panel` 用 `clip`（表头要粘住、行要从下面滚过），所以卡片常态给 `var(--shadow-1)` 就好，悬停抬升只给不裁剪的面：弹窗、气泡、`bg-card` 的 React 区块。想让面板也能抬升就得改 `overflow` 策略，会影响所有宽表格的滚动锚定——**风险太大，不要顺手改**，要改先跟用户讲清楚。

**Tailwind v4 两个会误判的现象：**
- `inline` 模式下 `var(--color-primary-lite)` **不会**出现在产物里，它被内联成 `var(--ui-primary-lite)`。别去追「缺失」的令牌。
- 没有任何类用到的 CSS 变量会被 JIT 摇掉。新加 `--shadow-lift` 在真有组件用它之前，产物里查不到，这是正常的。

## 注释也要跟着改

本仓库的注释写的是**「为什么」，包括否掉了哪种做法**。行为一变，原注释立刻变成假话，必须同一个提交里修掉。实际清掉过的假话：「面板不投影」「平色不叠 --glow」「--shadow-1 会糊成一片灰边」。留着比没有更糟。

## 编码：所有编辑走 node 脚本

`desktop-tauri/ui/css/layout.css` 里有一段 **GBK 混排**注释，导致 Edit 工具按 UTF-8 匹配失败。PowerShell 的 `[IO.File]::ReadAllLines` + `WriteAllLines` 也可能把中文写坏。

规矩：
- 改这些文件用本 skill 的 `scripts/find.mjs` 定位 + node 脚本替换，**不要**用 PowerShell 的 `Get-Content` / `Set-Content`
- 每轮改完跑 `check-encoding.mjs` 确认无损（查 BOM、CJK 字符数、`???` 替换痕迹、UTF-8 roundtrip）
- 写文件一律 `fs.writeFileSync(path, s, 'utf8')`，不带 BOM

## 辅助脚本

三个脚本都在 `.opencode/skills/ui-reference-redesign/scripts/`，**以下命令都在仓库根执行**：

| 脚本 | 作用 |
| --- | --- |
| `find.mjs <正则> [条数上限=20] [子目录]` | 搜源码。子目录相对**仓库根**（`desktop-tauri/ui/islands`，不是 `ui/islands`） |
| `check-encoding.mjs [文件或目录...]` | 编码体检。不给参数就查默认那批 CSS |
| `sync-system-theme.mjs [--check] [文件...]` | 从 `dark` 块重新生成 `system` 块。`--check` 只校验不写 |

`find.mjs` 不受 PowerShell 引号转义干扰——中文正则里带 `'` `"` 时，shell 会把参数拆坏，脚本里读到的 pattern 跟你敲的不一样。

## 脚本本身踩过的坑（照抄时别再犯）

写这几个脚本时踩的，全是**静默失败**——不报错，只是结果悄悄错了：

- **被 PowerShell 拆坏的参数。** `find.mjs "className='stat"` 在 PS 下拿到的是 `className=stat`。脚本内部一律用 `process.argv`，别在 shell 里做引号 gymnastics。
- **遮罩注释再找选择器。** `theme.css` 的文档注释里出现过 `[data-theme='dark']` 字面量，直接正则搜会命中注释里的假选择器。必须先把注释内容替换成空格再扫。
- **引号状态机 + 属性选择器。** 扫到 `'` 或 `"` 要跳过整段字符串，但 `[data-theme="dark"]` 里的引号**不是**字符串起始。先吃掉 `[...]` 就能避开；不处理就会把后面的真选择器边界算错。
- **括号配对而非正则找块尾。** 用 `indexOf('}')` 找块的结束位置，遇到嵌套（块里有 `@media`、注释里有 `}`）就错位。必须数 `{`/`}` 深度。
- **缩进从选择器所在行量，不是从 `{` +1 量。** 后者是块体首行；块体首行要是多行注释的续行（这个仓库的注释是对齐的长中文块），能量出比选择器深好几格的缩进，然后整块重建就缩歪了。
- **重建时保留块内的相对缩进和空行。** 逐行 `trim()` + 统一缩进会把多行注释的续行对齐拍平、把声明之间的空行吃掉。
- **归一化 vs 逐字节一致。** `system` 块是 `dark` 整体缩进两级的副本，**永远不可能**与 `dark` 字节相同。所以判断「排版有差异」不能用 `a !== b`（恒真，等于一句废话），要比注释内容序列。这个脚本的验收契约是**声明一致 + 幂等**，不是字节还原。
- **`--check` 通过 ≠ 无需修复。** 只比声明的话，注释和排版差异会被静默放过；反过来提示「跑一次修复」而修复因为提前 `return` 什么都不做，是更糟的撒谎。
- **`find.mjs` 目录写错要报错退出。** 静默扫到 0 个文件看起来像「没有匹配」，实际是路径写错了。
- **`find.mjs` 显式传子目录时不要跳过构建产物。** 默认跳过 `ui.js` / `ui.css`，但传了子目录就意味着明确要搜它们——`ui/islands` 里就这两个文件。

## 验证阶梯

改完按这个顺序过，每层都能独立抓一类错：

```bash
S=.opencode/skills/ui-reference-redesign/scripts
node $S/find.mjs "<关键字>" 20                  # 1. 定位（不受 shell 引号转义干扰）
node $S/check-encoding.mjs <改过的文件>         # 2. 编码无损
node $S/sync-system-theme.mjs --check           # 3. 改了令牌文件才需要
npm --prefix desktop-tauri/ui-kit      run typecheck   # 4. 两包都过
npm --prefix desktop-tauri/ui-islands run typecheck
npm --prefix desktop-tauri/ui-islands run build       # 5. 产物里真的有新类/新令牌
node $S/find.mjs "stat-ico" 10 desktop-tauri/ui/islands   # 6. 在产物里确认落地
```

第 5 步必查：`ui/islands/ui.{css,js}` 是构建产物，**改动必须一起提交**，否则 CI 构建出来和本地不是一回事。

**产物是压缩过的**（`ui.css` 单行 68 KB、注释被剥掉），所以只改空白和缩进时产物字节不变，可以跳过重新构建——`tokens.css` 更是压根不进这个构建（`ui/css/*.css` 是 Tauri 直接打包的静态文件，不是 island 的输入）。真改了选择器或声明就必须构建。

## 提交

- 一次视觉改动一个 commit，提交信息写清「第几阶段 + 改了什么」，和历史风格一致（`UI 重构第五阶段：…`）。
- 推送后查 CI：`docker` 工作流是本项目的主工作流。
  ```powershell
  Invoke-RestMethod "https://api.github.com/repos/picgo91/agent2api/actions/runs?per_page=3" -Headers @{ "User-Agent"="opencode" }
  ```
- 发版流程不在这里，见 `agent.md`。