import { invoke } from '@tauri-apps/api/core'
import './styles.css'

interface Candidate {
  path: string
  size: number
  tier: string
  reason: string
  category: string
  blocked: boolean
  blocked_bytes: number
  rule_id: string
}

interface ScanView {
  root: string
  folders: number
  files: number
  bytes: number
  duration_ms: number
  candidates: Candidate[]
}

interface ExecResult {
  path: string
  status: string
  reason: string | null
  bytes: number
}

interface BatchView {
  batch: string
  bytes: number
  items: ExecResult[]
}

interface StagedItem {
  id: string
  src: string
  bytes: number
  status: string
}

const $ = <T extends HTMLElement>(id: string): T => document.getElementById(id) as T

const state: {
  scan: ScanView | null
  picked: Set<string>
  staged: StagedItem[]
} = { scan: null, picked: new Set(), staged: [] }

// ------------------------------------------------------------------ formatting

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB']

function bytes(value: number): string {
  let size = value
  let unit = 0
  while (size >= 1024 && unit < UNITS.length - 1) {
    size /= 1024
    unit += 1
  }
  return unit === 0 ? `${value} B` : `${size.toFixed(1)} ${UNITS[unit]}`
}

function thousands(value: number): string {
  return value.toLocaleString('en-US')
}

function prettyPath(path: string): string {
  const home = path.match(/^[a-z]:\\users\\[^\\]+/i)?.[0]
  return home ? '~' + path.slice(home.length) : path
}

function tierLabel(tier: string): string {
  return { SAFE_PURGE: 'Safe', REVIEW: 'Review', KEEP: 'Keep', LOCKED: 'Locked' }[tier] ?? tier
}

// --------------------------------------------------------------------- render

function renderSummary(): void {
  const host = $('summary')
  if (!state.scan) {
    host.innerHTML = ''
    return
  }

  const scan = state.scan
  const byTier = new Map<string, { count: number; total: number }>()
  for (const candidate of scan.candidates) {
    const entry = byTier.get(candidate.tier) ?? { count: 0, total: 0 }
    entry.count += 1
    entry.total += candidate.size
    byTier.set(candidate.tier, entry)
  }

  const cells = [
    ['folders', thousands(scan.folders)],
    ['files', thousands(scan.files)],
    ['size', bytes(scan.bytes)],
    ['scan', `${(scan.duration_ms / 1000).toFixed(1)}s`],
    ...['SAFE_PURGE', 'REVIEW', 'KEEP', 'LOCKED'].map((tier) => {
      const entry = byTier.get(tier)
      return [tierLabel(tier), entry ? `${entry.count} · ${bytes(entry.total)}` : '—']
    })
  ]

  host.innerHTML = cells
    .map(([label, value]) => `<div class="stat"><span>${label}</span><b>${value}</b></div>`)
    .join('')
}

function renderList(): void {
  const host = $('list')
  if (!state.scan) {
    host.innerHTML = `<p class="muted">Scan the drive to see what can be removed.</p>`
    return
  }
  if (state.scan.candidates.length === 0) {
    host.innerHTML = `<p class="muted">Nothing to review.</p>`
    return
  }

  host.innerHTML = state.scan.candidates
    .map((candidate) => {
      const selectable = candidate.tier === 'SAFE_PURGE' || candidate.tier === 'REVIEW'
      const picked = state.picked.has(candidate.path)
      const blocked = candidate.blocked
        ? `<span class="tag">holds ${bytes(candidate.blocked_bytes)} protected</span>`
        : ''
      return `
        <label class="row ${picked ? 'picked' : ''}">
          <input type="checkbox" data-path="${escapeAttr(candidate.path)}" ${picked ? 'checked' : ''} ${
            selectable ? '' : 'disabled'
          } />
          <i class="dot t-${candidate.tier}"></i>
          <div class="row-main">
            <div class="row-path">${escapeHtml(prettyPath(candidate.path))}${blocked}</div>
            <div class="row-why">${escapeHtml(candidate.reason)}</div>
          </div>
          <div class="row-size">${bytes(candidate.size)}</div>
        </label>`
    })
    .join('')

  for (const input of host.querySelectorAll<HTMLInputElement>('input[type=checkbox]')) {
    input.addEventListener('change', () => {
      const path = input.dataset.path ?? ''
      if (input.checked) state.picked.add(path)
      else state.picked.delete(path)
      renderFooter()
      renderList()
    })
  }
}

function renderFooter(): void {
  const picked = state.scan?.candidates.filter((candidate) => state.picked.has(candidate.path)) ?? []
  const total = picked.reduce((sum, candidate) => sum + candidate.size, 0)
  $('selection').textContent = `${picked.length} selected · ${bytes(total)}`
  ;($('quarantine') as HTMLButtonElement).disabled = picked.length === 0
}

function renderStaged(): void {
  const host = $('staged-list')
  const active = state.staged.filter((item) => item.status === 'quarantined' || item.status === 'missing')

  $('staged-total').textContent = active.length
    ? `${active.length} staged · ${bytes(active.reduce((sum, item) => sum + item.bytes, 0))}`
    : 'empty'

  ;($('restore') as HTMLButtonElement).disabled = active.length === 0
  ;($('purge') as HTMLButtonElement).disabled = active.length === 0

  host.innerHTML = state.staged
    .map(
      (item) => `
      <div class="row">
        <span></span>
        <i class="dot ${item.status === 'quarantined' ? 't-REVIEW' : 't-LOCKED'}"></i>
        <div class="row-main">
          <div class="row-path">${escapeHtml(prettyPath(item.src))}</div>
          <div class="row-why">${item.status}</div>
        </div>
        <div class="row-size">${bytes(item.bytes)}</div>
      </div>`
    )
    .join('')
}

function render(): void {
  renderSummary()
  renderList()
  renderFooter()
  renderStaged()
}

// ---------------------------------------------------------------------- toast

let toastTimer: number | undefined

function toast(message: string, tone: 'ok' | 'error' = 'ok'): void {
  const host = $('toast')
  host.textContent = message
  host.style.borderColor = tone === 'ok' ? '#26405a' : 'rgba(239,68,68,.5)'
  host.classList.remove('hidden')
  window.clearTimeout(toastTimer)
  toastTimer = window.setTimeout(() => host.classList.add('hidden'), 6000)
}

function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/g, (char) => `&#${char.charCodeAt(0)};`)
}

function escapeAttr(value: string): string {
  return escapeHtml(value)
}

// -------------------------------------------------------------------- actions

async function scan(): Promise<void> {
  const button = $('scan') as HTMLButtonElement
  button.disabled = true
  $('progress').classList.remove('hidden')
  $('progress-text').textContent = 'scanning C:\\ …'

  try {
    const started = performance.now()
    const view = await invoke<ScanView>('scan_drive', { path: null })
    state.scan = view
    state.picked.clear()
    $('subtitle').textContent = `${view.root} · ${thousands(view.folders)} folders · ${bytes(view.bytes)} · ${(
      (performance.now() - started) / 1000
    ).toFixed(1)}s`
    render()
  } catch (error) {
    toast(String(error), 'error')
  } finally {
    $('progress').classList.add('hidden')
    button.disabled = false
  }
}

async function quarantineSelected(): Promise<void> {
  const paths = [...state.picked]
  if (paths.length === 0) return

  try {
    const result = await invoke<BatchView>('quarantine_paths', { paths })
    const moved = result.items.filter((item) => item.status === 'quarantined')
    const refused = result.items.filter((item) => item.status !== 'quarantined')
    toast(
      `${moved.length} moved to quarantine · ${bytes(result.bytes)} — nothing deleted yet` +
        (refused.length ? ` · ${refused.length} skipped` : '')
    )
    state.picked.clear()
    await refreshStaged()
    await scan()
  } catch (error) {
    toast(String(error), 'error')
  }
}

async function refreshStaged(): Promise<void> {
  state.staged = await invoke<StagedItem[]>('staged_items')
  renderStaged()
}

async function restoreAll(): Promise<void> {
  try {
    const result = await invoke<BatchView>('restore_all')
    toast(`${result.items.filter((i) => i.status === 'restored').length} restored · ${bytes(result.bytes)}`)
    await refreshStaged()
  } catch (error) {
    toast(String(error), 'error')
  }
}

async function purgeAll(): Promise<void> {
  const active = state.staged.filter((item) => item.status === 'quarantined')
  const total = active.reduce((sum, item) => sum + item.bytes, 0)

  const confirmed = window.confirm(
    `Permanently delete ${active.length} folder(s), ${bytes(total)}?\n\nThere is no undo after this.`
  )
  if (!confirmed) return

  try {
    const result = await invoke<BatchView>('purge_all')
    toast(`${bytes(result.bytes)} reclaimed`)
    await refreshStaged()
  } catch (error) {
    toast(String(error), 'error')
  }
}

// ----------------------------------------------------------------------- boot

$('scan').addEventListener('click', () => void scan())
$('quarantine').addEventListener('click', () => void quarantineSelected())
$('restore').addEventListener('click', () => void restoreAll())
$('purge').addEventListener('click', () => void purgeAll())
$('select-safe').addEventListener('click', () => {
  const safe = state.scan?.candidates.filter((candidate) => candidate.tier === 'SAFE_PURGE') ?? []
  state.picked = new Set(safe.map((candidate) => candidate.path))
  renderList()
  renderFooter()
})

render()
void refreshStaged()
