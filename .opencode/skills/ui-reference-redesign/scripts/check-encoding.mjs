#!/usr/bin/env node
/**
 * 编码体检 —— 每轮改完 CSS/TSX 都要跑一遍。
 *
 * 为什么需要：PowerShell 的 `[IO.File]::WriteAllLines` 在这个仓库的 Windows 环境上
 * 会把中文注释写坏，而且**肉眼看不出来**：控制台把 UTF-8 渲染成乱码，
 * 你会以为只是显示问题，直到某天发现文件里的 `?` 已经落盘。
 * 判断标准是读原始字节算 roundtrip，不是看终端输出。
 *
 * 查四件事：
 *   BOM       —— 期望无 BOM；git 的 CRLF 警告常和它一起冒出来，无害但要知道
 *   CJK       —— 中文字符数，突然掉一大截就是被写坏了
 *   U+FFFD    —— 真损坏的信号（解码失败留下的替换字符）
 *   roundtrip —— 字节 → utf8 解码 → 字节，完全一致才算无损
 *
 * 用法：
 *   node check-encoding.mjs                        # 查默认那批文件
 *   node check-encoding.mjs a.css b.tsx           # 查指定文件
 *   node check-encoding.mjs desktop-tauri/ui/css   # 查整个目录
 */
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../..')

/** 本项目常被改动的 CSS —— 这些文件里有 GBK 混排注释，是编码事故的高发区 */
const DEFAULT = [
  'desktop-tauri/ui-kit/styles/theme.css',
  'desktop-tauri/ui/css/tokens.css',
  'desktop-tauri/ui/css/components.css',
  'desktop-tauri/ui/css/layout.css',
  'desktop-tauri/ui/css/page-report.css',
]

const EXT = /\.(css|tsx?|html|json|md)$/i
const SKIP_DIR = /(^|[\\/])(node_modules|target|dist|\.git)([\\/]|$)/

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
    else if (EXT.test(e.name)) out.push(p)
  }
  return out
}

const args = process.argv.slice(2)
const targets = args.length
  ? args.flatMap(a => {
      const abs = path.resolve(ROOT, a)
      return fs.existsSync(abs) && fs.statSync(abs).isDirectory() ? walk(abs) : [abs]
    })
  : DEFAULT.map(p => path.join(ROOT, p))

let bad = 0
for (const file of targets) {
  if (!fs.existsSync(file)) {
    console.log(`SKIP  ${path.relative(ROOT, file)}  (不存在)`)
    continue
  }
  const buf = fs.readFileSync(file)
  const text = buf.toString('utf8')
  const bom = buf[0] === 0xef && buf[1] === 0xbb && buf[2] === 0xbf
  const cjk = (text.match(/[\u4e00-\u9fff]/g) || []).length
  const repl = (text.match(/\ufffd/g) || []).length
  const clean = Buffer.from(text, 'utf8').equals(buf)

  const problems = []
  if (repl > 0) problems.push(`U+FFFD x${repl} —— 内容真的坏了`)
  if (!clean) problems.push('roundtrip 不一致 —— 写入过程有损')
  if (bom) problems.push('有 BOM')
  if (problems.length) bad++

  console.log(
    (problems.length ? 'FAIL  ' : 'OK    ') +
      path.relative(ROOT, file).replace(/\\/g, '/').padEnd(42) +
      `CJK=${String(cjk).padStart(5)}  BOM=${bom ? 'yes' : 'no'}` +
      (problems.length ? '  ← ' + problems.join('；') : '')
  )
}

console.log(bad === 0 ? '\n编码全部无损' : `\n${bad} 个文件有问题，别提交`)
process.exit(bad === 0 ? 0 : 1)