#!/usr/bin/env node
/**
 * 从 dark 块重新生成 system 块。
 *
 * 本仓库的主题文件是三块结构：
 *   :root { ... }                                   亮色
 *   :root[data-theme="dark"] { ... }                深色（跟随主题时也用它）
 *   @media (prefers-color-scheme: dark) {
 *     :root[data-theme="system"] { ... }            跟随系统 —— 必须和 dark 完全一致
 *   }
 *
 * 第三块是**冗余**的，但两份手抄迟早会漂移（加一个 `--primary-lite` 忘了同步，
 * 表现就是「切到深色主题颜色对，切到跟随系统颜色不对」这类极难查的问题）。
 * 所以：只改前两块，第三块跑这个脚本重生成，然后自动比对。
 *
 * ── 为什么定位要先做遮罩 ──────────────────────────────────
 * theme.css 的文件头注释里**提到过** `:root[data-theme="dark"]` 这几个字
 * （在解释主题机制）。直接在原文里 indexOf 会命中注释，然后往后找 `{`
 * 就会捡到完全无关的下一个块 —— 曾因此对着一个 169 字符的假块报「已一致」。
 * 所以先做一份等长的遮罩副本（注释和字符串内容换成空格，索引与原文一一对应），
 * 选择器定位和括号配对都在遮罩上做，取内容才回到原文。
 *
 * 用法：
 *   node sync-system-theme.mjs                       # 默认那两个令牌文件
 *   node sync-system-theme.mjs desktop-tauri/ui/css/tokens.css
 *   node sync-system-theme.mjs --check a.css b.css    # 只校验不写
 */
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../..')

const DEFAULT = [
  'desktop-tauri/ui-kit/styles/theme.css',
  'desktop-tauri/ui/css/tokens.css',
]

/**
 * 等长遮罩：注释与字符串的**内容**换成空格（保留换行），索引与原文完全对齐。
 * 于是遮罩上搜到的位置可以安全地拿去切原文。
 *
 * 属性选择器要单独跳过：`[data-theme='dark']` 里的带引号值长得和字符串一模一样，
 * 不跳过的话遮罩器会把 `'dark'` 当成字符串、把 dark 三个字遮掉，
 * 于是后面按 dark 找块必然找不到，然后「跳过」——看上去没事，实际一次都没校验。
 */
function mask(text) {
  const out = text.split('')
  let i = 0
  const blank = (from, to) => {
    for (let k = from; k < to; k++) if (out[k] !== '\n') out[k] = ' '
  }
  while (i < text.length) {
    const two = text.slice(i, i + 2)
    if (two === '/*') {
      const end = text.indexOf('*/', i + 2)
      const stop = end < 0 ? text.length : end + 2
      blank(i + 2, stop - 2)
      i = stop
    } else if (two === '//' && text[i + 2] !== '/') {
      const end = text.indexOf('\n', i)
      const stop = end < 0 ? text.length : end
      blank(i + 2, stop)
      i = stop
    } else if (text[i] === '[') {
      // 属性选择器整体跳过：里面既不会有注释，也不会有要挡的字符串
      const end = text.indexOf(']', i)
      i = end < 0 ? text.length : end + 1
    } else if (text[i] === '"' || text[i] === "'") {
      const quote = text[i]
      let j = i + 1
      while (j < text.length && text[j] !== quote) j += text[j] === '\\' ? 2 : 1
      blank(i + 1, Math.min(j, text.length))
      i = j + 1
    } else {
      i++
    }
  }
  return out.join('')
}

/** 在遮罩上做括号配对，返回 { selAt, open, close }（原文索引；open 是 `{`，close 是 `}`）
 *
 *  两个必须写对的细节，少一个就会**静默地一个块都匹配不上**，而脚本会报「跳过、
 *  一切正常」——比报错危险得多：
 *
 *  1. 选择器用正则不用字面量：**两个令牌文件引号风格不一致**，
 *     theme.css 写 `:root[data-theme='dark']`（单引号），tokens.css 写双引号。
 *  2. 闭合引号后面还有属性选择器的 `]`：`[data-theme="dark"]`。
 *     漏掉这个 `\]`，`['"]dark['"]\s*\{` 就永远匹配不上（下一个字符是 `]` 不是空白）。
 *
 *  `selAt` 是必需的：重建 system 块时缩进要从**选择器所在行**量，
 *  而 `open + 1` 已经是块体首行 —— 从那儿量会拿到块体的缩进（4 格）而不是
 *  选择器的（2 格），差一级；块体首行若是注释续行还能量出 7 格。
 */
function blockBody(masked, theme) {
  const selAt = masked.search(new RegExp(`:root\\[data-theme=['"]${theme}['"]\\]\\s*\\{`))
  if (selAt < 0) return null
  const open = masked.indexOf('{', selAt)
  let depth = 0
  for (let i = open; i < masked.length; i++) {
    if (masked[i] === '{') depth++
    else if (masked[i] === '}') {
      depth--
      if (depth === 0) return { selAt, open, close: i }
    }
  }
  return null
}

/** 归一化后比对：空白、行尾、注释都不算差异 */
function normalize(body) {
  return body.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\s+/g, ' ').trim()
}

function sync(file, check) {
  const abs = path.resolve(ROOT, file)
  const name = path.basename(file)
  if (!fs.existsSync(abs)) {
    console.log(`SKIP  ${name}  (不存在)`)
    return true
  }
  const original = fs.readFileSync(abs, 'utf8')
  const text = original.replace(/\r\n/g, '\n')
  const masked = mask(text)

  const dark = blockBody(masked, 'dark')
  const system = blockBody(masked, 'system')
  if (!dark || !system) {
    // 文件里明显有主题机制却定位不到块 = 定位逻辑坏了，不是「这个文件不需要」
    // 这种情况必须 FAIL：静默 SKIP 会让一个从没被校验的文件看起来一直很健康
    if (/data-theme/.test(masked)) {
      console.log(`FAIL  ${name}  文件里有 data-theme 但定位不到 dark/system 块 —— 选择器或引号风格变了，改脚本，别当成「不需要」`)
      return false
    }
    console.log(`SKIP  ${name}  (这个文件没有主题块，跳过)`)
    return true
  }

  const darkBody = text.slice(dark.open + 1, dark.close)
  const sysBody = text.slice(system.open + 1, system.close)

  if (process.env.SYNC_DEBUG) {
    const at = i => text.slice(0, i).split('\n').length
    console.log(`      [debug] ${name}`)
    console.log(`        dark   行 ${at(dark.open)}..${at(dark.close)}  ${darkBody.split('\n').length} 行`)
    console.log(`        system 行 ${at(system.open)}..${at(system.close)}  ${sysBody.split('\n').length} 行`)
  }

  // 断言块里真有自定义属性声明。没有这条就会把「括号配对配到了别的块」
  // 这种错误判成一致 —— 那是校验脚本最坏的失败方式：静默地什么都没验。
  const decls = (normalize(darkBody).match(/--[\w-]+\s*:/g) || []).length
  if (decls < 3) {
    console.log(`FAIL  ${name}  dark 块里只找到 ${decls} 个 --声明，多半是括号配到了错误的块，别自动改，人工确认`)
    return false
  }

  /** 取出注释正文序列（忽略缩进与空白）。
 *  不能用「剥掉注释后比原文」—— system 是 dark 整体缩进两级的副本，
 *  剥掉注释后两边仍然因为缩进不等，永远判成「有差异」，等于一句废话。 */
const commentsOf = body =>
  (body.match(/\/\*[\s\S]*?\*\//g) || []).map(c => c.replace(/\s+/g, ' ').trim())

const n = normalize(darkBody).length
  if (normalize(darkBody) === normalize(sysBody)) {
    // 声明一致。到这里为止**缩进必然是不同的**（system 是 dark 整体缩进两级的副本），
    // 所以不能用「原文不等」来判断排版差异 —— 那会永远成立，变成一句废话。
    // 只有注释的有无才是真信息。
    const commentsDiffer =
      JSON.stringify(commentsOf(darkBody)) !== JSON.stringify(commentsOf(sysBody))
    if (check) {
      console.log(
        `OK    ${name.padEnd(16)}system 与 dark 声明一致（${decls} 个声明 / ${n} 字符）` +
          (commentsDiffer ? '  ← 注释有差异（不影响生效；不带 --check 跑一次可对齐）' : '')
      )
      return true
    }
    // 非 --check：即使声明一致也继续往下走，把注释与排版一并对齐。
    // 否则上面那句提示就是假的 —— 说「跑一次修复」而修复会直接 return 什么也不做。
  } else if (check) {
    console.log(`FAIL  ${name.padEnd(16)}system 与 dark 不一致，跑一次不带 --check 的修复`)
    return false
  }

  // 重建 body。两条容易踩的：
  //
  // 1. 首尾各留一个换行。`system.open` 是 `{` 的位置、`+1` 才是块体起点，
  //    也就是原文 `{` 之后紧跟的那个 `\n` 属于被替换掉的那一段。不补换行的话
  //    重建出来的第一行会直接贴在 `{` 后面，变成 `... {  --bg: ...`。
  // 2. **保留块内的相对缩进和空行**，只把整块平移到目标缩进。
  //    逐行 trim + 统一缩进会把多行注释的续行对齐拍平（这个仓库的注释是
  //    精心对齐的中文长块，拍平后很难读），也会丢掉声明之间的空行。
  const selLineStart = text.lastIndexOf('\n', system.selAt) + 1
  const selIndent = (text.slice(selLineStart, system.selAt).match(/^[ \t]*/) || [''])[0].length
  const sysIndent = selIndent + 2

  const indentOf = l => (l.match(/^[ \t]*/) || [''])[0].length
  const lines = darkBody.split('\n')
  while (lines.length && !lines[0].trim()) lines.shift()
  while (lines.length && !lines[lines.length - 1].trim()) lines.pop()
  const firstReal = lines.find(l => l.trim())
  const darkBase = firstReal ? indentOf(firstReal) : 0

  const body = lines
    .map(line => {
      if (!line.trim()) return ''
      return ' '.repeat(sysIndent + Math.max(0, indentOf(line) - darkBase)) + line.trim()
    })
    .join('\n')
  const rebuilt = '\n' + body + '\n' + ' '.repeat(selIndent)
  const next = text.slice(0, system.open + 1) + rebuilt + text.slice(system.close)
  fs.writeFileSync(abs, next, 'utf8')

  const okNow = normalize(darkBody) === normalize(rebuilt)
  console.log(
    `${okNow ? 'OK   ' : 'FAIL '} ${name.padEnd(16)}已从 dark 重生成 system（${decls} 个声明 / ${normalize(rebuilt).length} 字符）`
  )
  return okNow
}

const argv = process.argv.slice(2)
const check = argv.includes('--check')
const files = argv.filter(a => a !== '--check')
const list = files.length ? files : DEFAULT

const allOk = list.map(f => sync(f, check)).every(Boolean)
console.log(allOk ? '\n三块主题一致' : '\n有不一致的地方')
process.exit(allOk ? 0 : 1)