// 前端（原生 HTML/JS，无框架）跨文件共享的全局符号声明。
//
// ── 为什么需要这个文件 ─────────────────────────────────────────
// ui/*.js 是**经典全局脚本**（`<script src>` 顺序加载，不是 ES module），
// 各文件通过挂 `window.wbXxx` 互相暴露接口。TypeScript 在 `checkJs` 模式下
// 看不到这些运行时约定，于是每一个 `window.wbApp.foo` 都报 TS2339。
//
// 这里把它们统一声明为 `any`：目的是让 `@ts-check` **在这些文件内部**能
// 抓真正的问题（DOM API 用法、局部变量、拼写、可空性），而不是给跨文件
// 的全局契约做完整类型建模（那是后续可以逐步收紧的事）。
//
// 注意：纯 `var`/`function` 形式的顶层全局（如 app.js 的 `$` / `toast` /
// `busy` / `releaseBusy` / `refresh`）**不在这里**声明 —— tsconfig 关掉了
// moduleDetection 的模块化，它们本来就共享在同一个全局作用域里，直接可见。
// 只有「挂到 window 上的属性」和「JS 里凭空出现的裸标识符」才需要下面这段。
//
// ── 维护方式 ──────────────────────────────────────────────────
// 新增一个 `window.wbXxx = {...}` 的模块时，在下面 `Window` 接口里补一行。
// 未声明的全局会被 `tsc -p desktop-tauri/ui` 报出来（这正是 CI 想拦的）。

/** 各文件挂到 window 上的命名空间统一按 `any` 处理（见文件头说明）。 */
interface Window {
  wbApp: any;
  wbReport: any;
  wbConfirm: any;
  wbAccountPanel: any;
  wbAccountsModel: any;
  wbAccountsView: any;
  wbAliyunCaptcha: any;
  wbAutoclawOauth: any;
  wbCodeArtsWelfare: any;
  wbColSettings: any;
  wbConversationPreview: any;
  wbCustomProvidersUi: any;
  wbFilterMemory: any;
  wbIcons: any;
  wbKeysPanel: any;
  wbLogsPanel: any;
  wbMarkdown: any;
  wbModelsPanel: any;
  wbPortPanel: any;
  wbPresetProviders: any;
  wbProviders: any;
  wbProxiesPanel: any;
  wbRequestsPanel: any;
  wbSettingsPanel: any;
  wbSmsLogin: any;
  wbTableColumns: any;
  wbTasksPanel: any;
  wbTooltip: any;
  wbUnits: any;
  wbUpdatePanel: any;
  wbUpgradePanel: any;
  wbWebLogin: any;
  wbZcodeCaptchaPool: any;
  wbZcodeClaim: any;
  /** Tauri 壳注入的桥（桌面端）；网页端由 web_shim.rs 注入同形对象。 */
  workbuddyDesktop: any;
  __TAURI_INTERNALS__: any;
}

// 有的文件把命名空间当裸标识符用（`wbApp.foo` 而不是 `window.wbApp.foo`），
// 但它们只作为 window 属性存在，所以这里补一层裸声明。
declare var wbApp: any;
declare var wbReport: any;
declare var wbConfirm: any;
declare var wbIcons: any;
declare var workbuddyDesktop: any;
