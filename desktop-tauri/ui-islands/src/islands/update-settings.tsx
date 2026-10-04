import * as React from 'react'
import {
  Badge,
  Button,
  Dialog,
  DialogBody,
  DialogContent,
  DialogHeader,
  DialogTitle,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  cn,
} from '@ui'
import {
  buildProxyPick, errorMessage, fetchProxySelection, openExternal, saveProxy,
  shared, toast, type ProxySelection, type UpdateTokenStatus,
  EMPTY_PROXY_SELECTION,
} from './update-shared'

/**
 * 「更新设置」弹窗（设置页「软件更新」面板头部那颗按钮打开）。
 *
 * ── 两块设置，各自的保存时机 ────────────────────────────────
 *   · 出网代理：下拉**选中即保存**（与账号页代理列同一交互，省一个确认键），
 *     检查更新与下载安装包立即走新线路；
 *   · GitHub 令牌：粘贴 → 点「保存」（有明确输入动作的设置就该有明确确认）。
 *
 * ── 令牌「已填写」的口径 ─────────────────────────────────────
 * 保存成功后**界面永远不再显示令牌本体**（后端 `/api/update/token` 只回
 * filled / origin，想回显也拿不到）—— 重新打开弹窗看到的只有一枚「已填写」
 * 徽章。想换令牌就再粘一次（覆盖保存），想撤销用「清除」。
 *
 * ── 排版 ──────────────────────────────────────────────────
 * 弹窗只放控件与一行状态说明，成段的解释全部收进悬停提示与面板顶部的 tip：
 * 弹窗是「改设置」的地方，不是读文档的地方。底部也不放「关闭」—— 组件库
 * Dialog 自带右上角 ✕，Esc / 点遮罩同样能关。
 *
 * ── 状态归属 ───────────────────────────────────────────────
 * 两块设置的读数都是本组件的本地状态（每次打开现拉），不进 update-panel 的
 * 模块快照：面板那套快照服务的是「检查 / 下载」的命令式流程，弹窗跟着开跟着
 * 关，塞进去只会让两棵树多一层没必要的耦合。类型与读写函数在 update-shared.ts。
 */

/**
 * 快捷跳转：GitHub 令牌创建页，query 预填「备注」。
 *
 * **刻意不预选任何权限**（不带 scopes）：无权限（no scopes）的经典令牌就能读
 * 公开仓库的发布信息 —— 本项目检查的正是它 —— 并把限额提到 5000 次/小时；
 * 预选 repo 反而是过度授权（那是私有仓库的完整读写）。用户打开页面直接点
 * 底部的 Generate token 即可。
 */
const GITHUB_TOKENS_URL = 'https://github.com/settings/tokens/new?description='
  + encodeURIComponent('AIapi 更新检查')

/** 令牌一栏的状态徽章 + 一句说明（完整原因放徽章的悬停提示里，不占版面） */
function tokenStatus(token: UpdateTokenStatus | null): { badge: React.ReactNode; hint: string } {
  if (!token) {
    return { badge: <Badge shape='tag' variant='outline'>读取中</Badge>, hint: '' }
  }
  if (token.error) {
    return {
      badge: <Badge shape='tag' variant='destructive' title={token.error}>无法解密</Badge>,
      hint: '重新粘贴保存一次即可自愈',
    }
  }
  if (token.filled && token.origin === 'stored') {
    return {
      badge: <Badge shape='tag' variant='success'>已填写</Badge>,
      hint: '已加密存储在本地，不显示具体值；粘贴新令牌可覆盖',
    }
  }
  if (token.filled) {
    return {
      badge: <Badge shape='tag' variant='secondary'>环境变量</Badge>,
      hint: '已配置 GITHUB_TOKEN 环境变量，界面保存的令牌优先于它',
    }
  }
  return {
    badge: <Badge shape='tag' variant='outline'>未填写</Badge>,
    hint: '填写后检查限额 60 → 5000 次/小时（创建令牌无需勾选任何权限）',
  }
}

export function UpdateSettingsDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [selection, setSelection] = React.useState<ProxySelection>(EMPTY_PROXY_SELECTION)
  const [token, setToken] = React.useState<UpdateTokenStatus | null>(null)
  const [draft, setDraft] = React.useState('')
  const [busy, setBusy] = React.useState(false)
  const pick = buildProxyPick(selection)

  // 每次打开都现拉两块设置的读数：值可能被上一轮弹窗或环境改过，
  // 两条请求都便宜，不值得为它做缓存
  React.useEffect(() => {
    if (!open) return
    setDraft('')
    void fetchProxySelection().then(setSelection)
    void (async () => {
      try {
        setToken((await shared().workbuddyDesktop?.getUpdateToken()) ?? null)
      } catch {
        // 读不到按「未知」显示（token 保持 null →「读取中」徽章），保存动作会带出新状态
      }
    })()
  }, [open])

  async function handlePick(value: string): Promise<void> {
    const result = await saveProxy(value) // 失败在内部 toast；成功也顺带播报
    if (result) setSelection(prev => ({ ...prev, proxyChoice: result.choice }))
  }

  async function saveToken(): Promise<void> {
    const value = draft.trim()
    if (!value) {
      toast('请先粘贴 GitHub 令牌', 'err')
      return
    }
    setBusy(true)
    try {
      const result = await shared().workbuddyDesktop?.setUpdateToken({ token: value })
      setToken(result ?? null)
      setDraft('')
      if (result?.saved === false) toast('令牌已生效，但写入磁盘失败（重启后会丢失）', 'err')
      else toast('✅ GitHub 令牌已保存（加密存储，界面不再显示）')
    } catch (error) {
      toast(`保存失败：${errorMessage(error)}`, 'err')
    } finally {
      setBusy(false)
    }
  }

  async function clearToken(): Promise<void> {
    setBusy(true)
    try {
      const result = await shared().workbuddyDesktop?.setUpdateToken({ token: null })
      setToken(result ?? null)
      toast('✅ 已清除界面保存的 GitHub 令牌（环境变量若配置过则继续生效）')
    } catch (error) {
      toast(`清除失败：${errorMessage(error)}`, 'err')
    } finally {
      setBusy(false)
    }
  }

  const status = tokenStatus(token)
  const stored = Boolean(token?.error) || token?.origin === 'stored'
  const placeholder = token?.filled && token?.origin !== 'env'
    ? '已填写（粘贴新令牌可覆盖）'
    : '粘贴 GitHub 令牌（ghp_… / github_pat_…）'

  return (
    // 受控 open（面板按 settingsOpen 条件渲染本组件）：关窗一律由 onClose 收口，
    // Esc / 点遮罩 / 右上角 ✕ 由 Base UI Dialog 内建
    <Dialog open={open} onOpenChange={next => { if (!next) onClose() }}>
      {/* 默认 620px 对这两节内容太宽（右侧一截空白），收窄成一个紧凑的设置小窗 */}
      <DialogContent className='w-[min(460px,calc(100vw-48px))]'>
        <DialogHeader>
          <DialogTitle>更新设置</DialogTitle>
        </DialogHeader>
        {/* DialogBody 自带 gap-4，这里收紧到 gap-3.5；每节是一个子元素，
            节内间距自己控（不与 gap 叠加） */}
        <DialogBody className='gap-3.5'>
          {/* 出网代理：说明都在下拉的悬停提示里，这里只留标题与控件 */}
          <div>
            <div className='text-[13px] font-semibold text-foreground'>出网代理</div>
            <div className='mt-1.5'>
              <Select value={pick.current} onValueChange={value => void handlePick(String(value))}>
                {/* 线路不可用时描边标红（照账号页 ProxyCell 的口径），原因看 title */}
                <SelectTrigger
                  className={cn('w-full', pick.broken && 'border-destructive-bd')}
                  title={pick.title}
                  aria-label='更新出网代理'
                >
                  <SelectValue className='min-w-0 truncate'>{pick.selected?.label || '直连'}</SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {pick.items.map(item => (
                    <SelectItem key={item.value} value={item.value} disabled={item.disabled}>{item.label}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          </div>

          <div className='h-px bg-hairline' />

          {/* GitHub 令牌：标题行右侧就是快捷跳转（预设好备注、无需勾选权限） */}
          <div>
            <div className='flex items-center justify-between gap-2'>
              <div className='text-[13px] font-semibold text-foreground'>GitHub 令牌</div>
              <Button
                variant='outline'
                size='xs'
                title='在默认浏览器中打开 GitHub 的令牌创建页（备注已预设，无需勾选任何权限，直接点 Generate token）'
                onClick={() => void openExternal(GITHUB_TOKENS_URL)}
              >
                打开 GitHub 令牌页面
              </Button>
            </div>
            <div className='mt-1.5 flex items-center gap-2'>
              {status.badge}
              <span className='text-[12px] text-subtle'>{status.hint}</span>
            </div>
            <div className='mt-2 flex items-center gap-2'>
              {/* 密码形态：防肩窥；不回显是后端口径，这里只是不把粘贴值展示成明文 */}
              <Input
                type='password'
                className='flex-1'
                placeholder={placeholder}
                autoComplete='new-password'
                spellCheck={false}
                value={draft}
                onChange={event => setDraft(event.currentTarget.value)}
                onKeyDown={event => {
                  if (event.key === 'Enter') {
                    event.preventDefault()
                    void saveToken()
                  }
                }}
              />
              <Button variant='default' size='sm' disabled={busy || !draft.trim()} onClick={() => void saveToken()}>
                保存
              </Button>
              {stored ? (
                <Button variant='outline' size='sm' disabled={busy} onClick={() => void clearToken()}>
                  清除
                </Button>
              ) : null}
            </div>
          </div>
        </DialogBody>
      </DialogContent>
    </Dialog>
  )
}
