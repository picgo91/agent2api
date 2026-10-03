#!/usr/bin/env node
/**
 * 定位用 grep —— 存在的理由是 PowerShell 的引号转义。
 *
 * `Select-String -Pattern 'a|b'` 里的单引号、双引号、反斜杠会被 PowerShell 自己先吃掉一层，
 * 写到第三个就变成 ParserError。与其每次和转义搏斗，不如用 node。
 *
 * 另外两个本仓库特有的行为：
 *   - 默认跳过注释行（`//`、`*`、`<!--`），搜索结果里全是「这个关键字出现在注释里」没用
 *   - 路径相对仓库根打印，不是相对脚本目录
 *
 * 用法：
 *   node find.mjs "className='stat"        # 正则
 *   node find.mjs "empty" 20               # 限 20 条
 *   node find.mjs "\.th\b" 10 ui/css       # 只搜某个子目录
 */
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../..')

const [pattern, limitArg = '20', sub] = process.argv.slice(2)
if (!pattern) {
  console.error('用法: node find.mjs <正则> [条数上限=20] [子目录]')
  process.exit(1)
}
const limit = Number(limitArg) || 20

const DEFAULT_EXT = /\.(tsx?|css|html|json|md|rs|yml|yaml|js)$/i
const SKIP_DIR = /(^|[\\/])(node_modules|target|dist|\.git)([\\/]|$)/
/** 构建产物：搜源码时默认排除。
 *  **显式传了子目录就不排除** —— 否则 `find.mjs "x" 5 ui/islands` 会扫到 0 个文件，
 *  因为那个目录里只有 ui.js / ui.css，而它们正是要搜的东西。显式指定等于明确要求。 */
const SKIP_FILE = /(^|[\\/])(ui\.js|ui\.css|ui-kit\.js|ui-kit\.css)$/

function walk(dir, out = []) {
  let entries
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true })
  } catch {
    return out
  }
  for (const e of entries) {
    const p = path.join(dir, e.name)
    if (SKIP_DIR.test(p)) continue
    if (e.isDirectory()) walk(p, out)
    else if (DEFAULT_EXT.test(e.name) && (sub || !SKIP_FILE.test(p))) out.push(p)
  }
  return out
}

const searchRoot = sub ? path.join(ROOT, sub) : ROOT
if (sub && !fs.existsSync(searchRoot)) {
  // 静默扫到 0 个文件是最糟的失败方式：看起来「没有匹配」，实际是路径写错了。
  // 子目录是相对**仓库根**的，不是相对 desktop-tauri。
  console.error(`目录不存在: ${sub}\n  解析成 ${searchRoot}\n  子目录要相对仓库根，例如 desktop-tauri/ui/islands`)
  process.exit(1)
}
const files = walk(searchRoot)
const re = new RegExp(pattern, 'i')

const WIDTH = 150

/** 展示命中处的上下文。
 *  压缩产物（`ui.js` / `ui.css`）整份文件只有一行，动辄 70 万字符 ——
 *  只截行首的话，看到的是离命中点 70 万字符的地方，等于没给上下文。 */
function snippet(line, m) {
  if (line.length <= WIDTH) return line
  const at = m ? m.index : 0
  // 命中点在中间就以其为中心截，两端都不够就贴着行首/行尾
  let start = Math.max(0, at - Math.floor(WIDTH / 3))
  if (start + WIDTH > line.length) start = Math.max(0, line.length - WIDTH)
  const cut = text => (start > 0 ? '…' : '') + text + (start + WIDTH < line.length ? '…' : '')
  return cut(line.slice(start, start + WIDTH))
}

let hits = 0
for (const file of files) {
  const lines = fs.readFileSync(file, 'utf8').replace(/\r\n/g, '\n').split('\n')
  for (let i = 0; i < lines.length && hits < limit; i++) {
    const t = lines[i].trim()
    const m = t.match(re)
    if (!m) continue
    // 整行都是注释就跳过（压缩产物里没有注释，不用担心误伤）
    if (!t.includes('/*') && (t.startsWith('*') || t.startsWith('//') || t.startsWith('<!--'))) continue
    console.log(
      path.relative(ROOT, file).replace(/\\/g, '/') + ':' + (i + 1) + '  ' + snippet(t, m)
    )
    hits++
  }
}
console.log(`\n(扫了 ${files.length} 个文件，命中展示 ${hits} 条${hits >= limit ? '，可能还有' : ''})`)