/**
 * Agent2API · 登录 / 首次注册页（ui/login.html）—— React 岛。
 *
 * 这一页独立于主面板（没有 index.html 那套骨架，也不属于任何 .page），界面整块由
 * 本岛渲染。但**登录逻辑仍归页面底部那段内联脚本**：它按 id 直接读写
 * #username / #password / #submit / #error / #mode-tip，还把 submit 处理器挂在
 * #form 上（capture 阶段，先于官方 widget 的 form 集成）。所以本岛刻意做成
 * 「渲染一次就不再更新」—— 不持 state、不重渲染，脚本对这些节点的命令式改动
 * （按钮文案与禁用、错误行、占位符、提示行）才不会被 React 覆盖回去。
 * 动这个文件之前请先读那段脚本，它是这一页真正的行为来源。
 *
 * 首次提交必须同步（flushSync）：脚本是紧随本岛之后的经典脚本，求值时就要拿到
 * 这些节点（`widget.addEventListener` 拿到 null 会直接抛错，整段脚本作废、点登录
 * 毫无反应）；而 createRoot().render() 默认交给调度器异步提交，同步脚本会跑在提交
 * 之前、拿到空 DOM。代价是本页的 `<script src="islands/ui.js">` 必须排在 #login-app
 * 之后，且不能加 defer/async —— 加了顺序就散了。
 *
 * ── 视觉：参考 admin.aook.eu.org/login 的「深底 + 白卡」────────────────
 * 页面级样式（整页底色、氛围层、入场与抖动动画）在 login.html 的 <style> 里；
 * 卡片与表单在本岛。颜色一律走组件库令牌，本页 data-theme 固定 light（卡片要白底
 * 浅色档文字，理由见 login.html 那条注释）。
 *
 * 参考页有、这里刻意没搬的四样，以及原因：
 *   1. Google Fonts 的 Inter —— 桌面端与 Docker 部署都要能离线用，不引外链字体。
 *   2. 按钮里的登录图标 —— #submit 的文案由底部脚本用 textContent 覆写
 *      （'登录' / '创建账号并进入面板'），任何子节点都会被抹掉。同理 #mode-tip、
 *      #error 都不能放图标。
 *   3. 环境光的靛/紫 —— 参考页 indigo→violet 落在蓝 500 的卡片后面会带出紫色偏色，
 *      与全站蓝色主色打架。login.html 里按品牌蓝族重映射了色相，保留其结构。
 *   4. 24px 大圆角 —— 本项目控件圆角在 e47ee2f 一轮主动从 16px 收到 8px（扁平化）。
 *      只有这张卡与它的输入框回到参考页的 24/12px，其余控件不动。
 */

import * as React from 'react'
import { flushSync } from 'react-dom'
import { createRoot } from 'react-dom/client'
import { Button, InputGroup, InputGroupAddon, InputGroupInput, Label } from '@ui'

/** 挂载点 id：login.html 里唯一的岛容器 */
const HOST_ID = 'login-app'

/**
 * 输入框左侧图标。**不用 emoji**（字形是字体提供的，跨机器粗细与基线都不同，无法
 * 精确着色），沿用本文件既有的内联 SVG 写法：24 视口、stroke 走 currentColor，
 * 由 InputGroupAddon 的颜色驱动，聚焦时变主色（规则在 login.html）。
 */
function FieldIcon({ d }: { d: React.ReactNode }) {
  return (
    <svg
      viewBox='0 0 24 24'
      aria-hidden='true'
      className='size-4 shrink-0'
      fill='none'
      stroke='currentColor'
      strokeWidth='1.9'
      strokeLinecap='round'
      strokeLinejoin='round'
    >
      {d}
    </svg>
  )
}

/**
 * ALTCHA 官方 widget：第三方自定义元素，协议见 server::altcha。
 *
 * 用 createElement 而不是 JSX 标签：TS 的 IntrinsicElements 里没有这个标签，而为一个
 * 第三方标签去扩全局 JSX 命名空间会波及其他岛（也可能与并行迁移的文件撞车）。
 * React 对自定义元素的属性就是 setAttribute（true 写成空属性、字符串原样写入），
 * 与手写 HTML 等价 —— widget 正是靠 getAttribute 读 strings / hidefooter 的。
 *
 * 文案包逐字搬自改写前的 login.html；`hidefooter` 隐藏 altcha.org 的外链 footer。
 */
const altchaWidget = React.createElement('altcha-widget', {
  id: 'altcha',
  challengeurl: '/api/panel/captcha',
  name: 'altcha',
  hidefooter: true,
  strings:
    '{"aria":"我不是机器人","label":"我不是机器人","verified":"验证成功","verifying":"验证中…","wait":"请稍候…","error":"验证失败，请重试","expired":"验证已过期，请重试","footer":"由 ALTCHA 保护"}',
})

/** 渐变 id：本页只渲染一次，用常量避免每次 render 造新 id（会打断 SVG 引用） */
const MARK_GRADIENT_ID = 'login-brand-gradient'

/**
 * 卡片外壳与其中的表单。
 *
 * 五个 id 一个都不能少（见文件头）。#mode-tip 与 #error 渲染成空节点：文案由底部
 * 脚本填，这里只提供位置与占位高度（min-h-5 让错误行出现前后布局不跳）；
 * 抖动动画挂在 login.html 的 `#error:not(:empty)` 上，脚本一填文案就触发。
 */
function LoginPage() {
  return (
    // 玻璃卡：参考页的 rgba(255,255,255,0.95) + backdrop-blur(20px) + 24px 圆角
    // + 0 32px 80px 大投影。白卡压深底的实测对比度 17.85，卡片边界很「浮」。
    // 底色那一半是 `.login-card`（login.html 的普通 CSS）而非工具类 —— 见那里
    // 的注释：Tailwind 会把 `bg-card/95` 和任意值 color-mix 的 alpha 一并丢掉。
    <div className='login-card w-[min(420px,100%)] rounded-[24px] border border-white/60 p-[40px_32px] shadow-[0_32px_80px_rgba(0,0,0,0.30)] backdrop-blur-[20px]'>
      {/* 品牌标：保留本项目的箭头造型（与桌面端 / 面板侧栏同一 glyph），套参考页的
          渐变方块 + 外圈光环 + 呼吸投影。光环与脉冲是 login.html 里 .brand-mark 的
          两个伪元素（形状随 52px 对齐），这里只管 svg 本体。
          渐变两端都走令牌且白字都过 AA：--ui-primary ≈ #2563eb（5.17）、
          --ui-primary-hover ≈ #1d4ed8（6.70）。原先写死的 #2563eb 实底由此变成渐变。 */}
      <div className='mb-[28px] flex flex-col items-center gap-3.5 text-center'>
        <span className='brand-mark'>
          <svg viewBox='0 0 24 24' role='img' aria-label='Agent2API' className='size-[52px]'>
            <defs>
              <linearGradient id={MARK_GRADIENT_ID} x1='0' y1='0' x2='1' y2='1'>
                <stop offset='0' stopColor='var(--ui-primary)' />
                <stop offset='1' stopColor='var(--ui-primary-hover)' />
              </linearGradient>
            </defs>
            <rect width='24' height='24' rx='5.4' fill={`url(#${MARK_GRADIENT_ID})`} />
            <g fill='none' stroke='#fff' strokeWidth='2.2' strokeLinecap='round' strokeLinejoin='round'>
              <path d='M5.23 9.24h10.71' />
              <path d='M15.94 6.72 18.77 9.24 15.94 11.76' />
              <path d='M18.77 14.76H8.06' />
              <path d='M8.06 12.24 5.23 14.76 8.06 17.28' />
            </g>
          </svg>
        </span>
        <div>
          <h1 className='m-0 text-[22px] font-extrabold tracking-[-0.4px]'>Agent2API</h1>
          <div className='text-[13.5px] text-muted-foreground'>OpenAI 兼容网关 · 管理面板</div>
        </div>
      </div>

      <div id='mode-tip' className='mb-[20px] text-center text-[13.5px] text-muted-foreground' />

      <form id='form' autoComplete='on'>
        <Label htmlFor='username' className='mb-[6px] block text-[12.5px] font-medium'>
          账号
        </Label>
        {/* h-11 / rounded-[12px] 覆盖 InputGroup 默认的 30px 与 --r-md：
            参考页的输入框约 44px 高、12px 圆角，是这套观感的组成部分。
            边框与底色仍由 InputGroup 的令牌（control-border / control）给。 */}
        <InputGroup className='h-11 rounded-[12px]'>
          <InputGroupAddon>
            <FieldIcon
              d={
                <>
                  <circle cx='12' cy='8' r='3.4' />
                  <path d='M4.8 20c0-3.4 3.2-5.6 7.2-5.6s7.2 2.2 7.2 5.6' />
                </>
              }
            />
          </InputGroupAddon>
          <InputGroupInput
            id='username'
            autoComplete='username'
            placeholder='管理员账号'
            className='text-[14px]'
          />
        </InputGroup>

        <Label htmlFor='password' className='mt-4 mb-[6px] block text-[12.5px] font-medium'>
          密码
        </Label>
        <InputGroup className='h-11 rounded-[12px]'>
          <InputGroupAddon>
            <FieldIcon
              d={
                <>
                  <rect x='4.8' y='10.6' width='14.4' height='9.4' rx='2.2' />
                  <path d='M8.4 10.6V7.9a3.6 3.6 0 0 1 7.2 0v2.7' />
                </>
              }
            />
          </InputGroupAddon>
          <InputGroupInput
            id='password'
            type='password'
            autoComplete='new-password'
            placeholder='至少 8 位'
            className='text-[14px]'
          />
        </InputGroup>

        {/* 勾一下「我不是机器人」→ 后台算题 → 绿勾已验证 */}
        {altchaWidget}

        {/* type='submit' 必须显式给：Base UI 的 useButton 会给原生 button 补一个
            type="button"，只有显式传入（合并时外部 props 优先）才盖得掉。
            from-primary to-primary-hover 覆盖 variant 默认的 primary-lite→primary：
            静止态浅端 #3b82f6 压白字只有 3.68（不过 AA），登录是本页唯一的主动作，
            换成两端都过 AA 的那一对（5.17 / 6.70）。hover 位移放大到 2px，同参考页。
            代价是这一颗比全站主按钮略深 —— 要回一致就删掉这两个类。 */}
        <Button
          id='submit'
          type='submit'
          size='lg'
          className='h-11 w-full rounded-[12px] from-primary to-primary-hover text-[14px] hover:-translate-y-[2px]'
        >
          继续
        </Button>
        <div id='error' className='mt-3 min-h-5 text-center text-[13px] whitespace-pre-wrap text-destructive' />
      </form>

      <div className='mt-[24px] text-center text-[12px] text-muted-foreground'>
        登录后可在「网关 Key」页为 API 客户端创建密钥
      </div>
    </div>
  )
}

/**
 * 挂载。主面板里没有 #login-app（岛随同一个 bundle 一起加载），直接跳过 ——
 * 这是预期行为，不是漏挂。
 */
function mount(): void {
  const host = document.getElementById(HOST_ID)
  if (!host) return
  // 同步提交，理由见文件头
  flushSync(() => {
    createRoot(host).render(<LoginPage />)
  })
}

mount()