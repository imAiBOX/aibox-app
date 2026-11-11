#!/usr/bin/env node
// prepush-scan — 白名单推送/部署复检（凭据 + 内部文档 + 进展措辞）
// 用法: node scripts/prepush-scan.mjs [--staged | --tree | --history]
//   --staged   扫 origin/main..HEAD 的 diff + 工作树 tracked 文件（pre-push hook 默认）
//   --tree     只扫工作树 tracked 文件
//   --history  扫全历史（含 commit message；CI 用，需 fetch-depth: 0）
// 命中 → 打印 文件:行 + 模式名（绝不回显命中内容本身）→ exit 1
import { execSync } from 'node:child_process';

const mode = process.argv[2] || '--staged';

// 内容模式（值不回显，只报模式名与位置）
const PATTERNS = [
  { name: 'cloudflare-token', re: /cfat_[A-Za-z0-9_-]{20,}/g },
  { name: 'github-pat', re: /github_pat_[A-Za-z0-9_]{20,}/g },
  { name: 'github-token', re: /gh[pousr]_[A-Za-z0-9]{30,}/g },
  { name: 'telegram-bot-token', re: /\b\d{6,}:AA[A-Za-z0-9_-]{30,}/g },
  { name: 'private-key', re: /-----BEGIN (?:RSA |EC |DSA |OPENSSH )?PRIVATE KEY-----/g },
  { name: 's3-secret-key', re: /\b[0-9a-f]{40}\b(?=[^\n]*\baws_secret|secret_access)/gi },
  // 内部文档引用（文件名级）
  { name: 'internal-doc-ref', re: /PHASE\d+-[A-Z][A-Z-]*\.md|PRODUCT-PLAN-v\d|PHASE1-DO\u0044|PHASE1-DESI\u0047N|PHASE1-CONTRAC\u0054/g },
  // 内部进展措辞（公开面不应出现的进度/方案类措辞）
  { name: 'internal-progress-word', re: /\u65b9\u6848\u4e66|\u4efb\u52a1\u4e66|批次[0-9A-D]|C\u603b|DoD \u6e05\u5355|\u5185\u90e8\u4fe1\u606f/g },
];
// 内部文档路径（文件名级，扫 ls-files / diff --name-only）
const INTERNAL_PATH_RE = /(PHASE\d+-[A-Z]|PRODUCT-PLAN|DOD-CHECKLIST|CONTRACT-AUDIT|internal)/i;

function git(args, opts = {}) {
  return execSync(`git ${args}`, { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, ...opts });
}

function scanContent(label, text, findings) {
  for (const p of PATTERNS) {
    p.re.lastIndex = 0;
    const lines = text.split('\n');
    for (let i = 0; i < lines.length; i++) {
      p.re.lastIndex = 0;
      if (p.re.test(lines[i])) {
        findings.push({ pattern: p.name, where: `${label}:${i + 1}` });
        break; // 每文件每模式报一次即可
      }
    }
  }
}

/** 语言策略：根 README.md 必须以英文为主（可选中文在 README.zh-CN.md）；语言切换行豁免 */
function checkReadmeEnglish(label, text, findings) {
  if (!/(^|\/)README\.md$/.test(label)) return;
  const lines = text.split('\n').filter((l) => !/English\s*\|/.test(l) && !/\[English\]\(README\.md\)/.test(l));
  const m = lines.join('').match(/[\u4e00-\u9fff]/g);
  if (m && m.length > 40) findings.push({ pattern: 'readme-not-english', where: `${label} (${m.length} CJK chars in main README)` });
}

function scanPathList(label, paths, findings) {
  for (const p of paths) {
    if (p && INTERNAL_PATH_RE.test(p)) findings.push({ pattern: 'internal-doc-path', where: `${label} → ${p}` });
  }
}

const findings = [];
const tracked = git('ls-files').split('\n').filter(Boolean);

if (mode === '--tree' || mode === '--staged') {
  for (const f of tracked) {
    try {
      const text = git(`show :"${f}" 2>/dev/null`) || '';
      if (text) { scanContent(f, text, findings); checkReadmeEnglish(f, text, findings); }
    } catch { /* 未 staged 的新文件用磁盘内容 */ 
      try {
        const text = git(`show HEAD:"${f}"`);
        scanContent(f, text, findings);
        checkReadmeEnglish(f, text, findings);
      } catch { /* binary or missing */ }
    }
  }
  scanPathList('tracked-paths', tracked, findings);
}

if (mode === '--staged') {
  // 将要推送的 commit diff（相对远端 main）
  let range = 'origin/main..HEAD';
  try { git(`rev-parse --verify origin/main`); } catch { range = 'HEAD'; }
  try {
    const diff = git(`diff ${range}`);
    if (diff) scanContent(`diff(${range})`, diff, findings);
    const names = git(`diff --name-only ${range}`).split('\n').filter(Boolean);
    scanPathList(`diff-paths(${range})`, names, findings);
    const msgs = git(`log --format=%B ${range}`);
    if (msgs) scanContent(`messages(${range})`, msgs, findings);
  } catch (e) { console.error('[prepush-scan] staged diff failed:', e.message); process.exit(2); }
}

if (mode === '--history') {
  try {
    const all = git('log --all --format=%H');
    for (const sha of all.trim().split('\n').filter(Boolean)) {
      const msg = git(`log -1 --format=%B ${sha}`);
      scanContent(`msg ${sha.slice(0, 7)}`, msg, findings);
      const files = git(`ls-tree -r --name-only ${sha}`);
      scanPathList(`tree ${sha.slice(0, 7)}`, files.split('\n'), findings);
      if (files.split('\n').includes('README.md')) {
        try { checkReadmeEnglish(`tree ${sha.slice(0, 7)} README.md`, git(`show ${sha}:README.md`), findings); } catch {}
      }
    }
    // 全历史内容（含已删除文件的 blob）
    const patch = git('log --all -p --no-merges');
    scanContent('history-patch', patch, findings);
  } catch (e) { console.error('[prepush-scan] history scan failed:', e.message); process.exit(2); }
}

if (findings.length) {
  console.error(`[prepush-scan] ✗ 命中 ${findings.length} 处（不回显内容）：`);
  for (const f of findings) console.error(`  - [${f.pattern}] ${f.where}`);
  process.exit(1);
}
console.log(`[prepush-scan] ✓ ${mode} clean`);
