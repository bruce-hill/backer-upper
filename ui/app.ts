export {};

declare global {
  interface Window {
    __TAURI__: {
      core: {
        invoke: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
      };
    };
    toggleJob: (idx: number, enabled: boolean) => Promise<void>;
    editJob: (idx: number) => Promise<void>;
    togglePause: () => Promise<void>;
    cancelBackup: () => Promise<void>;
    showConfigFromBackup: () => Promise<void>;
    ejectDrive: () => Promise<void>;
    goToRestore: () => Promise<void>;
  }
}

const { invoke } = window.__TAURI__.core;

// ── Types ──────────────────────────────────────────────────────────────────────

interface DriveInfo {
  device: string;
  display_name: string;
  size?: string;
  tran?: string;
  fstype?: string;
  is_encrypted: boolean;
  is_mounted: boolean;
  dev_type: string;
  luks_parent?: string;
}

interface BackupJob {
  name: string;
  source: string;
  destination: string;
  excludes: string[];
  mode: string;
  enabled: boolean;
}

interface BackupConfig {
  jobs: BackupJob[];
  last_backup?: string;
}

interface AppStatus {
  config?: BackupConfig;
  config_dirty: boolean;
  mount_point?: string;
}

interface OpenDriveResult {
  error?: string;
  needs_password?: boolean;
  mounted?: {
    mount_point: string;
    config: BackupConfig;
  };
}

interface UnlockDriveResult {
  mount_point: string;
  config: BackupConfig;
}

interface BackupProgress {
  running: boolean;
  finished: boolean;
  finished_msg?: string;
  error?: string;
  paused: boolean;
  cancelled: boolean;
  job_name: string;
  current_file: string;
  overall_fraction: number;
  elapsed?: string;
  eta: string;
  log_lines: string[];
}

interface PreviewCommand {
  name: string;
  cmd: string;
}

interface RemoteDirEntry {
  name: string;
  is_dir: boolean;
  size: string;
}

interface TreeNode {
  name: string;
  path: string;
  isDir: boolean;
  size: string;
  children: TreeNode[] | null;
  loading: boolean;
  expanded: boolean;
}

interface DriveProbeResult {
  finished: boolean;
  lsblk_text?: string;
  note?: string;
  df_text?: string;
  ls_text?: string;
}

interface FormatProgress {
  step: number;
  total_steps: number;
  step_name: string;
  finished: boolean;
  error?: string;
  log: string[];
}

interface WipeProgress {
  bytes_written: number;
  total_bytes: number;
  finished: boolean;
  error?: string;
  cancelled: boolean;
}

// ── State ─────────────────────────────────────────────────────────────────────

let drives: DriveInfo[] = [];
let selectedDevice: string | null = null;
let editingJobIdx: number | null = null;
let treeSource = '';
let treeRoots: TreeNode[] = [];
let treeLoadError = '';
let treeLoadGen = 0;
let treeLoaded = false;

function getExcludeTextarea(): HTMLTextAreaElement {
  return document.getElementById('job-excludes-manual') as HTMLTextAreaElement;
}

function getTextareaLines(): string[] {
  return getExcludeTextarea().value.split('\n').map((s) => s.trim()).filter(Boolean);
}

function getTextareaExcludedPaths(): Set<string> {
  const presetPatternSet = new Set(PRESETS.flatMap((p) => p.patterns));
  const paths = new Set<string>();
  for (const line of getTextareaLines()) {
    if (!presetPatternSet.has(line) && line.startsWith('/') && !/[*?[{]/.test(line)) {
      paths.add(line.replace(/^\//, '').replace(/\/$/, ''));
    }
  }
  return paths;
}

const PRESETS: Array<{ id: string; label: string; title: string; patterns: string[] }> = [
  {
    id: 'cache',
    label: '~/.cache',
    title: 'Browser caches, thumbnails, app data caches (/.cache/)',
    patterns: ['/.cache/'],
  },
  {
    id: 'trash',
    label: 'Trash',
    title: 'Deleted files in ~/.local/share/Trash',
    patterns: ['/.local/share/Trash/'],
  },
  {
    id: 'node',
    label: 'node_modules',
    title: 'npm/yarn/pnpm dependency directories (node_modules/)',
    patterns: ['node_modules/'],
  },
  {
    id: 'python',
    label: 'Python',
    title: 'Bytecode and virtual environments (__pycache__/, *.pyc, .venv/)',
    patterns: ['__pycache__/', '*.pyc', '*.pyo', '.venv/', 'venv/'],
  },
  {
    id: 'rust',
    label: 'Rust build',
    title: 'Cargo compiler output (target/)',
    patterns: ['target/'],
  },
  {
    id: 'temp',
    label: 'Temp files',
    title: 'Temporary files and editor backups (*.tmp, *~, *.swp)',
    patterns: ['*.tmp', '*.temp', '*~', '*.swp', '*.swo'],
  },
  {
    id: 'steam',
    label: 'Steam',
    title: 'Game files — large, re-downloadable (/.local/share/Steam/)',
    patterns: ['/.local/share/Steam/'],
  },
  {
    id: 'osjunk',
    label: 'OS junk',
    title: 'macOS/Windows metadata files (.DS_Store, Thumbs.db)',
    patterns: ['.DS_Store', 'Thumbs.db'],
  },
];
let backupPollId: ReturnType<typeof setInterval> | null = null;
let formatPollId: ReturnType<typeof setInterval> | null = null;
let wipePollId: ReturnType<typeof setInterval> | null = null;
let probePollId: ReturnType<typeof setInterval> | null = null;
let formatDevice: string | null = null;
let formatIsDisk = false;
let operationIsRestore = false;
let pendingRestoreSnapshot: string | null = null;
let pendingRestoreJobIndices: number[] = [];
let pendingRestoreSubpaths: string[] = [];
let pendingRestoreDeleteExtra = false;

// ── Screen routing ────────────────────────────────────────────────────────────

function showScreen(name: string): void {
  document.querySelectorAll('.screen').forEach((s) => s.classList.remove('active'));
  document.getElementById('screen-' + name)!.classList.add('active');
  document.querySelector('main')!.scrollTop = 0;
}

function setStatusBar(msg: string, loading = false): void {
  const bar = document.getElementById('status-bar')!;
  if (loading) {
    bar.innerHTML = `<span class="spinner"></span> ${escHtml(msg)}`;
  } else {
    bar.textContent = msg || '';
  }
}

// ── Drive Select screen ───────────────────────────────────────────────────────

async function refreshDrives(): Promise<void> {
  try {
    drives = await invoke<DriveInfo[]>('list_drives');
  } catch (e) {
    drives = [];
    setStatusBar('Error listing drives: ' + e);
  }
  renderDriveList();
}

function renderDriveList(): void {
  const el = document.getElementById('drive-list')!;
  if (drives.length === 0) {
    el.innerHTML = '<div class="empty-state">No removable drives detected.</div>';
  } else {
    el.innerHTML = drives
      .map((d, i) => {
        const badges: string[] = [];
        if (d.tran) badges.push(`<span class="badge badge-usb">${escHtml(d.tran.toUpperCase())}</span>`);
        if (d.fstype && d.fstype !== 'crypto_LUKS')
          badges.push(`<span class="badge badge-fs">${escHtml(d.fstype)}</span>`);
        if (d.is_encrypted) badges.push('<span class="badge badge-luks">LUKS</span>');
        if (d.is_mounted) badges.push('<span class="badge badge-mounted">mounted</span>');
        const selected = d.device === selectedDevice ? ' selected' : '';
        return `
        <div class="drive-item${selected}" data-device="${d.device}" data-idx="${i}">
          <div>
            <div class="drive-name">${escHtml(d.display_name)}</div>
            <div class="drive-detail">${escHtml(d.device)}</div>
            <div class="badges">${badges.join('')}</div>
          </div>
          <div class="drive-size">${escHtml(d.size || '')}</div>
        </div>`;
      })
      .join('');

    el.querySelectorAll('.drive-item').forEach((item) => {
      const htmlItem = item as HTMLElement;
      htmlItem.addEventListener('click', () => {
        selectedDevice = htmlItem.dataset.device ?? null;
        renderDriveList();
        updateDriveSelectButtons();
      });
      htmlItem.addEventListener('dblclick', () => openSelectedDrive());
    });
  }
  updateDriveSelectButtons();
}

function updateDriveSelectButtons(): void {
  const drive = drives.find((d) => d.device === selectedDevice);
  (document.getElementById('btn-open-drive') as HTMLButtonElement).disabled = !drive;
  const canFormat = drive && (drive.dev_type === 'disk' || drive.dev_type === 'part');
  (document.getElementById('btn-format-drive') as HTMLButtonElement).disabled = !canFormat;
}

async function openSelectedDrive(): Promise<void> {
  if (!selectedDevice) return;
  setError('drive-select-error', '');
  try {
    const result = await invoke<OpenDriveResult>('open_drive', { device: selectedDevice });
    if (result.error) {
      setError('drive-select-error', result.error);
    } else if (result.needs_password) {
      const drive = drives.find((d) => d.device === selectedDevice);
      document.getElementById('password-heading')!.textContent =
        'Unlock: ' + (drive ? drive.display_name : selectedDevice);
      (document.getElementById('password-input') as HTMLInputElement).value = '';
      setError('password-error', '');
      showScreen('password');
      (document.getElementById('password-input') as HTMLInputElement).focus();
    } else if (result.mounted) {
      enterConfig(result.mounted.mount_point, result.mounted.config);
    }
  } catch (e) {
    setError('drive-select-error', String(e));
  }
}

// ── Password screen ───────────────────────────────────────────────────────────

async function unlockDrive(): Promise<void> {
  const pw = (document.getElementById('password-input') as HTMLInputElement).value;
  setError('password-error', '');
  (document.getElementById('btn-password-unlock') as HTMLButtonElement).disabled = true;
  try {
    const result = await invoke<UnlockDriveResult>('unlock_drive', {
      device: selectedDevice,
      password: pw,
    });
    enterConfig(result.mount_point, result.config);
  } catch (e) {
    setError('password-error', String(e));
  } finally {
    (document.getElementById('btn-password-unlock') as HTMLButtonElement).disabled = false;
  }
}

// ── Config Editor screen ──────────────────────────────────────────────────────

function enterConfig(mountPoint: string, config: BackupConfig): void {
  renderConfig(mountPoint, config);
  showScreen('config');
}

function renderConfig(mountPoint: string, config: BackupConfig): void {
  document.getElementById('config-mount-point')!.textContent = 'Drive: ' + mountPoint;
  if (config.last_backup) {
    const d = new Date(config.last_backup);
    document.getElementById('config-last-backup')!.textContent =
      'Last backup: ' + d.toLocaleString();
  } else {
    document.getElementById('config-last-backup')!.textContent = '';
  }
  renderJobsTable(config.jobs);
}

function renderJobsTable(jobs: BackupJob[]): void {
  const tbody = document.getElementById('jobs-tbody')!;
  if (jobs.length === 0) {
    tbody.innerHTML =
      '<tr><td colspan="5" style="text-align:center;color:var(--text-muted);padding:16px">No jobs configured.</td></tr>';
    return;
  }
  tbody.innerHTML = jobs
    .map(
      (j, i) => `
    <tr>
      <td>${escHtml(j.name)}</td>
      <td class="mono direction-cell">${escHtml(j.source)}<span class="dir-arrow">→</span>${escHtml(j.destination)}</td>
      <td>${j.mode === 'Backup' ? '📸 Snapshot' : j.mode === 'Media' ? '📚 Hoard' : escHtml(j.mode)}</td>
      <td>
        <input type="checkbox" ${j.enabled ? 'checked' : ''}
          onchange="toggleJob(${i}, this.checked)">
      </td>
      <td class="actions">
        <button onclick="editJob(${i})">Edit</button>
      </td>
    </tr>
  `
    )
    .join('');
}

async function toggleJob(idx: number, enabled: boolean): Promise<void> {
  const status = await invoke<AppStatus>('get_status');
  if (!status.config) return;
  const config = status.config;
  config.jobs[idx].enabled = enabled;
  await invoke('update_config', { config });
  await refreshConfigView();
}

async function refreshConfigView(): Promise<void> {
  const status = await invoke<AppStatus>('get_status');
  if (status.config) {
    renderJobsTable(status.config.jobs);
    const dirty = status.config_dirty;
    (document.getElementById('btn-save-config') as HTMLElement).style.display = dirty ? '' : 'none';
    (document.getElementById('unsaved-indicator') as HTMLElement).style.display =
      dirty ? '' : 'none';
  }
}

async function addJob(): Promise<void> {
  try {
    const config = await invoke<BackupConfig>('add_job');
    await editJob(config.jobs.length - 1);
  } catch (e) {
    setError('config-error', String(e));
  }
}

// ── Excludes file tree ────────────────────────────────────────────────────────

function isEffectivelyExcluded(path: string, excludedPaths: Set<string>): boolean {
  if (excludedPaths.has(path)) return true;
  const parts = path.split('/');
  for (let i = 1; i < parts.length; i++) {
    if (excludedPaths.has(parts.slice(0, i).join('/'))) return true;
  }
  return false;
}

function hasExcludedDescendant(node: TreeNode, excludedPaths: Set<string>): boolean {
  if (!node.children) return false;
  return node.children.some((c) => excludedPaths.has(c.path) || hasExcludedDescendant(c, excludedPaths));
}

function toggleTreeNode(path: string): void {
  const ta = getExcludeTextarea();
  const pattern = '/' + path;
  const excludedPaths = getTextareaExcludedPaths();
  let lines = getTextareaLines();
  if (excludedPaths.has(path)) {
    lines = lines.filter((l) => l !== pattern && l !== pattern + '/');
  } else if (!isEffectivelyExcluded(path, excludedPaths)) {
    lines.push(pattern);
    lines = lines.filter((l) => l === pattern || !l.startsWith(pattern + '/'));
  }
  ta.value = lines.join('\n');
  renderExcludesTree();
}

function findTreeNode(nodes: TreeNode[], path: string): TreeNode | null {
  for (const node of nodes) {
    if (node.path === path) return node;
    if (node.children && path.startsWith(node.path + '/')) {
      const found = findTreeNode(node.children, path);
      if (found) return found;
    }
  }
  return null;
}

async function toggleTreeExpand(path: string): Promise<void> {
  const node = findTreeNode(treeRoots, path);
  if (!node || !node.isDir || node.loading) return;
  node.expanded = !node.expanded;
  if (node.expanded && node.children === null) {
    node.loading = true;
    renderExcludesTree();
    try {
      const fullPath = treeSource.replace(/\/$/, '') + '/' + path;
      const entries = await invoke<RemoteDirEntry[]>('list_dir', { path: fullPath });
      node.children = entries.map((e) => ({
        name: e.name,
        path: path + '/' + e.name,
        isDir: e.is_dir,
        size: e.size,
        children: null,
        loading: false,
        expanded: false,
      }));
    } catch (_) {
      node.children = null;
      node.expanded = false;
    }
    node.loading = false;
  }
  renderExcludesTree();
}

async function loadExcludesTree(source: string, excludes: string[]): Promise<void> {
  const gen = ++treeLoadGen;
  treeSource = source.trim();
  treeRoots = [];
  treeLoaded = false;
  treeLoadError = '';
  getExcludeTextarea().value = excludes.join('\n');
  renderPresetChips();
  renderExcludesTree();
  if (!treeSource) return;

  try {
    const entries = await invoke<RemoteDirEntry[]>('list_dir', { path: treeSource });
    if (gen !== treeLoadGen) return;
    treeRoots = entries.map((e) => ({
      name: e.name,
      path: e.name,
      isDir: e.is_dir,
      size: e.size,
      children: null,
      loading: false,
      expanded: false,
    }));
  } catch (err) {
    if (gen !== treeLoadGen) return;
    treeLoadError = String(err);
  }
  treeLoaded = true;
  renderExcludesTree();
}

async function reloadTreeSource(source: string): Promise<void> {
  const gen = ++treeLoadGen;
  treeSource = source.trim();
  treeRoots = [];
  treeLoaded = false;
  treeLoadError = '';
  renderExcludesTree();
  if (!treeSource) return;
  try {
    const entries = await invoke<RemoteDirEntry[]>('list_dir', { path: treeSource });
    if (gen !== treeLoadGen) return;
    treeRoots = entries.map((e) => ({
      name: e.name,
      path: e.name,
      isDir: e.is_dir,
      size: e.size,
      children: null,
      loading: false,
      expanded: false,
    }));
  } catch (err) {
    if (gen !== treeLoadGen) return;
    treeLoadError = String(err);
  }
  treeLoaded = true;
  renderExcludesTree();
}

function renderExcludesTree(): void {
  const el = document.getElementById('excludes-tree')!;
  const scrollTop = el.scrollTop;

  if (!treeSource) {
    el.innerHTML = '<div class="tree-msg">Enter a source folder above to browse files</div>';
    return;
  }
  if (treeLoadError) {
    el.innerHTML = `<div class="tree-msg tree-error">${escHtml(treeLoadError)}</div>`;
    return;
  }
  if (!treeLoaded) {
    el.innerHTML = '<div class="tree-msg"><span class="spinner"></span> Loading directory…</div>';
    return;
  }
  if (treeRoots.length === 0) {
    el.innerHTML = '<div class="tree-msg">Source folder is empty</div>';
    return;
  }

  const excludedPaths = getTextareaExcludedPaths();
  el.innerHTML = renderTreeNodes(treeRoots, 0, excludedPaths);
  el.scrollTop = scrollTop;

  el.querySelectorAll<HTMLInputElement>('.tree-check[data-ind]').forEach((cb) => {
    cb.indeterminate = true;
  });
  el.querySelectorAll<HTMLInputElement>('.tree-check:not([disabled])').forEach((cb) => {
    cb.addEventListener('change', () => toggleTreeNode(cb.dataset.path!));
  });
  el.querySelectorAll<HTMLButtonElement>('.tree-toggle').forEach((btn) => {
    btn.addEventListener('click', () => toggleTreeExpand(btn.dataset.path!));
  });
}

function renderTreeNodes(nodes: TreeNode[], depth: number, excludedPaths: Set<string>): string {
  return nodes.map((n) => renderTreeNode(n, depth, excludedPaths)).join('');
}

function renderTreeNode(node: TreeNode, depth: number, excludedPaths: Set<string>): string {
  const effExcluded = isEffectivelyExcluded(node.path, excludedPaths);
  const directExcl = excludedPaths.has(node.path);
  const viaAnc = effExcluded && !directExcl;
  const indeterminate = !effExcluded && hasExcludedDescendant(node, excludedPaths);
  const checked = !effExcluded;

  const indent = 8 + depth * 20;
  const icon = node.isDir ? '📁' : '📄';

  const cbParts = [
    'type="checkbox"',
    'class="tree-check"',
    `data-path="${escHtml(node.path)}"`,
    checked || indeterminate ? 'checked' : '',
    viaAnc ? 'disabled' : '',
    indeterminate ? 'data-ind' : '',
  ].filter(Boolean);
  const cbAttrs = cbParts.join(' ');

  const toggleEl = node.isDir
    ? `<button class="tree-toggle" data-path="${escHtml(node.path)}">${node.expanded ? '▼' : '▶'}</button>`
    : `<span class="tree-toggle-ph"></span>`;

  let childrenHtml = '';
  if (node.isDir && node.expanded) {
    if (node.loading) {
      childrenHtml = `<div class="tree-children"><div class="tree-msg"><span class="spinner"></span></div></div>`;
    } else if (!node.children || node.children.length === 0) {
      childrenHtml = `<div class="tree-children"><div class="tree-msg">Empty directory</div></div>`;
    } else {
      childrenHtml = `<div class="tree-children">${renderTreeNodes(node.children, depth + 1, excludedPaths)}</div>`;
    }
  }

  return `<div class="tree-node${effExcluded ? ' is-excluded' : ''}">
    <div class="tree-row" style="padding-left:${indent}px">
      <input ${cbAttrs}>
      ${toggleEl}
      <span class="tree-icon">${icon}</span>
      <span class="tree-name">${escHtml(node.name)}</span>
      ${node.size ? `<span class="tree-size">${escHtml(node.size)}</span>` : ''}
    </div>
    ${childrenHtml}
  </div>`;
}

function renderPresetChips(): void {
  const lines = new Set(getTextareaLines());
  const el = document.getElementById('presets-chips')!;
  el.innerHTML = PRESETS.map(
    (p) =>
      `<button class="preset-chip${p.patterns.every((pat) => lines.has(pat)) ? ' active' : ''}" data-id="${escHtml(p.id)}" title="${escHtml(p.title)}">${escHtml(p.label)}</button>`
  ).join('');
  el.querySelectorAll<HTMLButtonElement>('.preset-chip').forEach((btn) => {
    btn.addEventListener('click', () => {
      const id = btn.dataset.id!;
      const preset = PRESETS.find((p) => p.id === id)!;
      const ta = getExcludeTextarea();
      let curLines = getTextareaLines();
      const lineSet = new Set(curLines);
      if (preset.patterns.every((p) => lineSet.has(p))) {
        const patSet = new Set(preset.patterns);
        curLines = curLines.filter((l) => !patSet.has(l));
      } else {
        for (const p of preset.patterns) {
          if (!lineSet.has(p)) curLines.push(p);
        }
      }
      ta.value = curLines.join('\n');
      renderPresetChips();
      renderExcludesTree();
    });
  });
}

function buildExcludes(): string[] {
  const seen = new Set<string>();
  const result: string[] = [];
  for (const line of getTextareaLines()) {
    if (!seen.has(line)) {
      seen.add(line);
      result.push(line);
    }
  }
  return result;
}

async function editJob(idx: number): Promise<void> {
  const status = await invoke<AppStatus>('get_status');
  const jobs = status.config?.jobs || [];
  const job = jobs[idx];
  editingJobIdx = idx;

  document.getElementById('job-edit-heading')!.textContent =
    idx < jobs.length ? 'Edit Job' : 'New Job';
  (document.getElementById('job-name') as HTMLInputElement).value = job?.name || '';
  (document.getElementById('job-source') as HTMLInputElement).value = job?.source || '';
  (document.getElementById('job-dest') as HTMLInputElement).value = job?.destination || '';
  (document.getElementById('job-enabled') as HTMLInputElement).checked = job?.enabled ?? true;
  const mode = job?.mode || 'Backup';
  document.querySelectorAll<HTMLInputElement>('input[name="job-mode"]').forEach((r) => {
    r.checked = r.value === mode;
  });
  (document.getElementById('btn-job-delete') as HTMLElement).style.display =
    idx < jobs.length ? '' : 'none';
  showScreen('job-edit');
  loadExcludesTree(job?.source || '', job?.excludes || []);
}

async function saveJob(): Promise<void> {
  const status = await invoke<AppStatus>('get_status');
  const config = status.config;
  if (!config) return;

  const excludes = buildExcludes();

  const modeEl = document.querySelector<HTMLInputElement>('input[name="job-mode"]:checked');
  const job: BackupJob = {
    name: (document.getElementById('job-name') as HTMLInputElement).value,
    source: (document.getElementById('job-source') as HTMLInputElement).value,
    destination: (document.getElementById('job-dest') as HTMLInputElement).value,
    excludes,
    mode: modeEl ? modeEl.value : 'Backup',
    enabled: (document.getElementById('job-enabled') as HTMLInputElement).checked,
  };

  if (editingJobIdx !== null && editingJobIdx < config.jobs.length) {
    config.jobs[editingJobIdx] = job;
  } else {
    config.jobs.push(job);
  }

  await invoke('update_config', { config });
  await invoke('save_config');
  const newStatus = await invoke<AppStatus>('get_status');
  enterConfig(newStatus.mount_point!, newStatus.config!);
  (document.getElementById('btn-save-config') as HTMLElement).style.display = newStatus.config_dirty ? '' : 'none';
  (document.getElementById('unsaved-indicator') as HTMLElement).style.display = newStatus.config_dirty ? '' : 'none';
}

async function deleteJob(): Promise<void> {
  if (editingJobIdx === null) return;
  try {
    await invoke<BackupConfig>('delete_job', { idx: editingJobIdx });
    await invoke('save_config');
    const status = await invoke<AppStatus>('get_status');
    if (!status.mount_point || !status.config) { showScreen('drive-select'); return; }
    enterConfig(status.mount_point, status.config);
    (document.getElementById('btn-save-config') as HTMLElement).style.display = status.config_dirty ? '' : 'none';
    (document.getElementById('unsaved-indicator') as HTMLElement).style.display = status.config_dirty ? '' : 'none';
  } catch (e) {
    alert('Delete failed: ' + e);
  }
}

async function saveConfig(): Promise<void> {
  try {
    await invoke('save_config');
    (document.getElementById('btn-save-config') as HTMLElement).style.display = 'none';
    (document.getElementById('unsaved-indicator') as HTMLElement).style.display = 'none';
    setStatusBar('Config saved.');
    setTimeout(() => setStatusBar(''), 2000);
  } catch (e) {
    setError('config-error', String(e));
  }
}

async function ejectDrive(): Promise<void> {
  const btn = document.getElementById('btn-eject') as HTMLButtonElement | null;
  if (btn) btn.disabled = true;
  setStatusBar('Ejecting…', true);
  if (wipePollId) { clearInterval(wipePollId); wipePollId = null; }
  try {
    await invoke('eject');
    selectedDevice = null;
    await refreshDrives();
    showScreen('drive-select');
  } catch (e) {
    setError('config-error', String(e));
    if (btn) btn.disabled = false;
  } finally {
    setStatusBar('');
  }
}

async function goToPreview(): Promise<void> {
  const status = await invoke<AppStatus>('get_status');
  if (status.config_dirty) {
    await invoke('save_config');
  }
  const cmds = await invoke<PreviewCommand[]>('preview_commands');
  const el = document.getElementById('preview-commands')!;
  const empty = document.getElementById('preview-empty')!;
  if (cmds.length === 0) {
    el.style.display = 'none';
    el.innerHTML = '';
    empty.style.display = '';
    (document.getElementById('btn-run-backup') as HTMLButtonElement).disabled = true;
  } else {
    el.style.display = '';
    empty.style.display = 'none';
    el.innerHTML = cmds
      .map(
        (c: PreviewCommand) => `
      <div class="preview-cmd-block">
        <div class="cmd-name">${escHtml(c.name)}</div>
        <pre>${escHtml(c.cmd)}</pre>
      </div>`
      )
      .join('');
    (document.getElementById('btn-run-backup') as HTMLButtonElement).disabled = false;
  }
  showScreen('preview');
}

// ── Backup screen ─────────────────────────────────────────────────────────────

async function startBackup(): Promise<void> {
  try {
    await invoke('start_backup');
  } catch (e) {
    alert('Failed to start backup: ' + e);
    return;
  }
  operationIsRestore = false;
  document.querySelector<HTMLElement>('#screen-backup h2')!.textContent = 'Backup';
  showScreen('backup');
  resetBackupUI();
  startBackupPoll();
}

function resetBackupUI(): void {
  (document.getElementById('backup-status-banner') as HTMLElement).style.display = 'none';
  document.getElementById('backup-job-info')!.textContent = '';
  setProgressBar(0, false, false);
  document.getElementById('backup-elapsed')!.textContent = '';
  document.getElementById('backup-log')!.innerHTML = '';
  _paused = false;
  const btnRow = document.getElementById('backup-btn-row')!;
  btnRow.innerHTML = `
    <button id="btn-backup-pause" onclick="togglePause()">Pause</button>
    <button id="btn-backup-cancel" onclick="cancelBackup()">Cancel</button>`;
}

let _paused = false;

async function togglePause(): Promise<void> {
  if (_paused) {
    await invoke('resume_backup');
    _paused = false;
    document.getElementById('btn-backup-pause')!.textContent = 'Pause';
  } else {
    await invoke('pause_backup');
    _paused = true;
    document.getElementById('btn-backup-pause')!.textContent = 'Resume';
  }
}

async function cancelBackup(): Promise<void> {
  await invoke('cancel_backup');
}

function startBackupPoll(): void {
  if (backupPollId) clearInterval(backupPollId);
  backupPollId = setInterval(pollBackup, 250);
}

async function pollBackup(): Promise<void> {
  const p = await invoke<BackupProgress>('get_backup_progress');
  updateBackupUI(p);
  if (!p.running && (p.finished || p.cancelled || p.error)) {
    clearInterval(backupPollId!);
    backupPollId = null;
  }
}

function updateBackupUI(p: BackupProgress): void {
  const banner = document.getElementById('backup-status-banner')!;
  const jobInfo = document.getElementById('backup-job-info')!;
  const elapsed = document.getElementById('backup-elapsed')!;

  const done = p.finished && !p.error;
  const hasErr = Boolean(p.error);
  setProgressBar(p.overall_fraction, hasErr, done);

  elapsed.textContent = p.elapsed ? 'Elapsed: ' + p.elapsed : '';

  if (p.error) {
    banner.className = 'banner danger';
    banner.textContent = 'Error: ' + p.error;
    banner.style.display = '';
    jobInfo.textContent = '';
  } else if (p.finished_msg) {
    banner.className = p.cancelled ? 'banner warning' : 'banner success';
    banner.textContent = p.finished_msg;
    banner.style.display = '';
    jobInfo.textContent = '';
  } else if (p.paused) {
    banner.className = 'banner warning';
    banner.textContent = 'Paused — ' + p.job_name;
    banner.style.display = '';
    jobInfo.textContent = '';
  } else {
    banner.style.display = 'none';
    jobInfo.innerHTML = p.running
      ? `<div class="job-info-row">
           <span class="job-info-name">${escHtml(p.job_name)}</span>
           <span class="job-info-file muted">${escHtml(p.current_file)}</span>
         </div>
         <div class="job-info-eta muted">ETA: ${escHtml(p.eta)}</div>`
      : '';
  }

  // Log
  const logEl = document.getElementById('backup-log')!;
  const wasAtBottom = logEl.scrollHeight - logEl.clientHeight <= logEl.scrollTop + 4;
  logEl.innerHTML = p.log_lines
    .map((l) => `<div class="${l.startsWith('>>>') ? 'log-cmd' : ''}">${escHtml(l)}</div>`)
    .join('');
  if (wasAtBottom) logEl.scrollTop = logEl.scrollHeight;

  // Swap buttons when done
  if (!p.running && (p.finished || p.cancelled || p.error)) {
    const btnRow = document.getElementById('backup-btn-row')!;
    const restoreAgainBtn = operationIsRestore
      ? `<button onclick="goToRestore()">↺ Restore Again</button>` : '';
    btnRow.innerHTML = `
      ${restoreAgainBtn}
      <button onclick="showConfigFromBackup()">← Back to Config</button>
      <button onclick="ejectDrive()" class="primary">Eject Drive</button>`;
  }
}

function setProgressBar(fraction: number, error: boolean, done: boolean): void {
  const bar = document.getElementById('backup-progress-bar')!;
  const label = document.getElementById('backup-progress-label')!;
  const pct = Math.round(fraction * 100);
  bar.style.width = pct + '%';
  label.textContent = pct + '%';
  bar.className = 'progress-bar' + (error ? ' error' : done ? ' done' : '');
}

async function showConfigFromBackup(): Promise<void> {
  const status = await invoke<AppStatus>('get_status');
  if (!status.mount_point || !status.config) { showScreen('drive-select'); return; }
  enterConfig(status.mount_point, status.config);
}

// ── Restore screen ────────────────────────────────────────────────────────────

async function goToRestore(): Promise<void> {
  const [snapshots, status] = await Promise.all([
    invoke<string[]>('list_snapshots').catch(() => [] as string[]),
    invoke<AppStatus>('get_status'),
  ]);
  const jobs = status.config?.jobs || [];

  const snapshotEl = document.getElementById('restore-snapshot-list')!;
  if (snapshots.length === 0) {
    snapshotEl.innerHTML = '<p class="muted">No snapshots available yet — complete a backup first to create one.</p>';
  } else {
    snapshotEl.innerHTML = snapshots.slice().reverse()
      .map((s, i) => `
      <label class="radio-option">
        <input type="radio" name="restore-snapshot" value="${escHtml(s)}" ${i === 0 ? 'checked' : ''} />
        <span>${snapshotLabel(s)}</span>
      </label>`)
      .join('');
  }

  const jobsEl = document.getElementById('restore-jobs-list')!;
  if (jobs.length === 0) {
    jobsEl.innerHTML = '<p class="muted">No jobs configured.</p>';
  } else {
    jobsEl.innerHTML = jobs
      .map(
        (j, i) => `
      <div class="restore-job-entry">
        <label class="radio-option">
          <input type="checkbox" class="restore-job-check" data-idx="${i}" checked />
          <span>
            <strong>${escHtml(j.name)}</strong>
            <span class="radio-desc">💾 <code>${escHtml(String(j.destination))}</code><span class="dir-arrow">→</span>💻 <code>${escHtml(j.source)}</code></span>
          </span>
        </label>
        <div class="restore-subpath-row">
          <span class="subpath-label">Subpath:</span>
          <input type="text" class="restore-job-subpath-input" data-idx="${i}"
                 placeholder="(entire job)" />
        </div>
      </div>`
      )
      .join('');
  }

  (document.getElementById('restore-delete-extra') as HTMLInputElement).checked = false;
  (document.getElementById('restore-confirm-input') as HTMLInputElement).value = '';
  setError('restore-error', '');
  (document.getElementById('restore-validation-msg') as HTMLElement).style.display = 'none';
  (document.getElementById('btn-do-restore') as HTMLButtonElement).disabled = true;

  showScreen('restore');
}

function validateRestore(): void {
  const confirm = (document.getElementById('restore-confirm-input') as HTMLInputElement).value;
  const hasSnapshot = document.querySelector<HTMLInputElement>('input[name="restore-snapshot"]:checked') !== null;
  const hasJobs = Array.from(
    document.querySelectorAll<HTMLInputElement>('.restore-job-check')
  ).some((cb) => cb.checked);

  const msgEl = document.getElementById('restore-validation-msg')!;
  if (confirm && confirm !== 'RESTORE') {
    msgEl.textContent = 'Type exactly: RESTORE';
    msgEl.style.display = '';
  } else {
    msgEl.textContent = '';
    msgEl.style.display = 'none';
  }

  (document.getElementById('btn-do-restore') as HTMLButtonElement).disabled = !(
    confirm === 'RESTORE' && hasSnapshot && hasJobs
  );
}

async function goToRestorePreview(): Promise<void> {
  const snapshotInput = document.querySelector<HTMLInputElement>(
    'input[name="restore-snapshot"]:checked'
  );
  pendingRestoreSnapshot = snapshotInput?.value || null;
  pendingRestoreJobIndices = Array.from(
    document.querySelectorAll<HTMLInputElement>('.restore-job-check')
  )
    .filter((cb) => cb.checked)
    .map((cb) => parseInt(cb.dataset.idx!, 10));
  pendingRestoreSubpaths = pendingRestoreJobIndices.map((idx) => {
    const input = document.querySelector<HTMLInputElement>(`.restore-job-subpath-input[data-idx="${idx}"]`);
    return input?.value.trim() ?? '';
  });
  pendingRestoreDeleteExtra = (document.getElementById('restore-delete-extra') as HTMLInputElement).checked;

  const btn = document.getElementById('btn-do-restore') as HTMLButtonElement;
  btn.disabled = true;
  btn.textContent = 'Running preview…';
  setError('restore-error', '');

  try {
    const lines = await invoke<string[]>('preview_restore', {
      snapshot: pendingRestoreSnapshot,
      jobIndices: pendingRestoreJobIndices,
      subpaths: pendingRestoreSubpaths,
      deleteExtra: pendingRestoreDeleteExtra,
    });

    const logEl = document.getElementById('restore-preview-log')!;
    const emptyEl = document.getElementById('restore-preview-empty')!;
    setError('restore-preview-error', '');

    const hasContent = lines.some((l) => l.trim());
    if (hasContent) {
      logEl.style.display = '';
      emptyEl.style.display = 'none';
      logEl.innerHTML = lines
        .map((l) => `<div class="${l.startsWith('>>>') ? 'log-cmd' : ''}">${escHtml(l)}</div>`)
        .join('');
      logEl.scrollTop = 0;
    } else {
      logEl.style.display = 'none';
      emptyEl.style.display = '';
    }

    showScreen('restore-preview');
  } catch (e) {
    setError('restore-error', String(e));
  } finally {
    btn.textContent = 'Preview Restore →';
    validateRestore();
  }
}

async function doRestore(): Promise<void> {
  setError('restore-preview-error', '');
  try {
    await invoke('start_restore', {
      snapshot: pendingRestoreSnapshot,
      jobIndices: pendingRestoreJobIndices,
      subpaths: pendingRestoreSubpaths,
      deleteExtra: pendingRestoreDeleteExtra,
    });
  } catch (e) {
    setError('restore-preview-error', String(e));
    return;
  }

  operationIsRestore = true;
  document.querySelector<HTMLElement>('#screen-backup h2')!.textContent = 'Restore';
  showScreen('backup');
  resetBackupUI();
  startBackupPoll();
}

// ── Format Setup screen ───────────────────────────────────────────────────────

async function enterFormatSetup(device: string, fstype: string | undefined, isDisk: boolean): Promise<void> {
  formatDevice = device;
  formatIsDisk = isDisk;

  const drive = drives.find((d) => d.device === device);
  const infoLine = drive
    ? `${drive.device} · ${drive.display_name} · ${drive.size || 'unknown size'}`
    : device;
  document.getElementById('format-drive-info-line')!.textContent = infoLine;
  document.getElementById('format-confirm-label')!.textContent = `Type ${device} to confirm`;
  (document.getElementById('format-confirm-input') as HTMLInputElement).placeholder = device;

  (document.getElementById('format-label') as HTMLInputElement).value = 'Backup';
  (document.getElementById('format-fstype') as HTMLSelectElement).value = 'btrfs';
  const luksEl = document.getElementById('format-luks') as HTMLInputElement;
  luksEl.checked = true;
  document.getElementById('format-luks-section')!.style.display = 'contents';
  (document.getElementById('format-pass1') as HTMLInputElement).value = '';
  (document.getElementById('format-pass2') as HTMLInputElement).value = '';
  (document.getElementById('format-confirm-input') as HTMLInputElement).value = '';
  setError('format-error', '');
  setError('format-validation-msg', '');
  (document.getElementById('btn-do-format') as HTMLButtonElement).disabled = true;

  buildFormatCmdPreview(device, isDisk);
  startProbe(device, fstype);
  showScreen('format-setup');
}

function buildFormatCmdPreview(device: string, isDisk: boolean): void {
  const label =
    (document.getElementById('format-label') as HTMLInputElement).value.trim() || '<label>';
  const fstype = (document.getElementById('format-fstype') as HTMLSelectElement).value || 'btrfs';
  const encrypt = (document.getElementById('format-luks') as HTMLInputElement).checked;
  invoke<string[]>('format_command_preview', { device, isDisk, label, fstype, encrypt }).then(
    (lines) => {
      document.getElementById('format-cmd-preview')!.innerHTML = lines
        .map((l) => `<div>${escHtml(l)}</div>`)
        .join('');
    },
  );
}

function startProbe(device: string, fstype: string | undefined): void {
  document.getElementById('format-probe-content')!.innerHTML =
    '<em style="color:#9ca3af">Reading drive contents…</em>';
  invoke('start_probe_drive', { device, fstype: fstype || null });
  if (probePollId) clearInterval(probePollId);
  probePollId = setInterval(pollProbe, 300);
}

async function pollProbe(): Promise<void> {
  const info = await invoke<DriveProbeResult>('get_drive_probe');
  if (!info.finished) return;
  clearInterval(probePollId!);
  probePollId = null;

  let html = '';
  if (info.lsblk_text) html += escHtml(info.lsblk_text) + '\n';
  if (info.note) html += '\n<span style="color:#fcd34d">' + escHtml(info.note) + '</span>\n';
  if (info.df_text)
    html += '\n<span style="color:#93c5fd">Disk usage (df -h):</span>\n' + escHtml(info.df_text);
  if (info.ls_text)
    html +=
      '\n<span style="color:#93c5fd">Top-level contents (ls -lAh):</span>\n' +
      escHtml(info.ls_text);
  document.getElementById('format-probe-content')!.innerHTML = html || '(no info)';
}

function validateFormat(): void {
  const label = (document.getElementById('format-label') as HTMLInputElement).value.trim();
  const luks = (document.getElementById('format-luks') as HTMLInputElement).checked;
  const p1 = (document.getElementById('format-pass1') as HTMLInputElement).value;
  const p2 = (document.getElementById('format-pass2') as HTMLInputElement).value;
  const confirm = (document.getElementById('format-confirm-input') as HTMLInputElement).value.trim();

  let msg = '';
  if (luks && p2 && p1 !== p2) msg = 'Passphrases do not match.';
  if (confirm && confirm !== formatDevice) msg = 'Must match exactly: ' + formatDevice;

  const msgEl = document.getElementById('format-validation-msg')!;
  if (msg) {
    msgEl.textContent = msg;
    msgEl.style.display = '';
  } else {
    msgEl.textContent = '';
    msgEl.style.display = 'none';
  }

  const passphraseOk = !luks || (p1 !== '' && p1 === p2);
  const ok = label && passphraseOk && confirm === formatDevice;
  (document.getElementById('btn-do-format') as HTMLButtonElement).disabled = !ok;
}

async function doFormat(): Promise<void> {
  const label = (document.getElementById('format-label') as HTMLInputElement).value.trim();
  const fstype = (document.getElementById('format-fstype') as HTMLSelectElement).value || 'btrfs';
  const encrypt = (document.getElementById('format-luks') as HTMLInputElement).checked;
  const passphrase = (document.getElementById('format-pass1') as HTMLInputElement).value;
  (document.getElementById('format-pass1') as HTMLInputElement).value = '';
  (document.getElementById('format-pass2') as HTMLInputElement).value = '';
  (document.getElementById('format-confirm-input') as HTMLInputElement).value = '';

  try {
    await invoke('start_format', {
      device: formatDevice,
      isDisk: formatIsDisk,
      label,
      fstype,
      encrypt,
      passphrase,
    });
  } catch (e) {
    setError('format-error', String(e));
    return;
  }

  showScreen('format-progress');
  (document.getElementById('format-progress-banner') as HTMLElement).style.display = 'none';
  document.getElementById('format-step-label')!.textContent = 'Starting…';
  document.getElementById('format-step-dots')!.innerHTML = '';
  document.getElementById('format-log')!.innerHTML = '';
  (document.getElementById('format-done-btn-row') as HTMLElement).style.display = 'none';

  if (formatPollId) clearInterval(formatPollId);
  formatPollId = setInterval(pollFormat, 300);
}

async function pollFormat(): Promise<void> {
  const p = await invoke<FormatProgress>('get_format_progress');
  updateFormatUI(p);
  if (p.finished) {
    clearInterval(formatPollId!);
    formatPollId = null;
  }
}

function updateFormatUI(p: FormatProgress): void {
  const banner = document.getElementById('format-progress-banner')!;
  if (p.error) {
    banner.className = 'banner danger';
    banner.textContent = 'Error: ' + p.error;
    banner.style.display = '';
  } else if (p.finished) {
    banner.className = 'banner success';
    banner.textContent = 'Formatting complete!';
    banner.style.display = '';
  }

  if (!p.error && !p.finished) {
    document.getElementById('format-step-label')!.textContent =
      `Step ${p.step} of ${p.total_steps}: ${p.step_name}`;
  } else {
    document.getElementById('format-step-label')!.textContent = '';
  }

  const dotsEl = document.getElementById('format-step-dots')!;
  if (p.total_steps > 0) {
    dotsEl.innerHTML = Array.from({ length: p.total_steps }, (_, i) => {
      const n = i + 1;
      let cls = 'step-dot';
      if (n < p.step || (n === p.step && p.finished && !p.error)) cls += ' done';
      else if (n === p.step) cls += p.error ? ' error' : ' active';
      return `<div class="${cls}">${n}</div>`;
    }).join('');
  }

  const logEl = document.getElementById('format-log')!;
  const wasAtBottom = logEl.scrollHeight - logEl.clientHeight <= logEl.scrollTop + 4;
  logEl.innerHTML = p.log
    .map((l) => `<div class="${l.startsWith('>>>') ? 'log-cmd' : ''}">${escHtml(l)}</div>`)
    .join('');
  if (wasAtBottom) logEl.scrollTop = logEl.scrollHeight;

  if (p.finished) {
    (document.getElementById('format-done-btn-row') as HTMLElement).style.display = '';
  }
}

// ── Wipe Free Space ───────────────────────────────────────────────────────────

async function startWipe(): Promise<void> {
  try {
    await invoke('start_wipe_free_space');
  } catch (e) {
    alert('Could not start wipe: ' + String(e));
    return;
  }
  showScreen('wipe');
  (document.getElementById('wipe-status-banner') as HTMLElement).style.display = 'none';
  document.getElementById('wipe-size-info')!.textContent = '';
  (document.getElementById('wipe-progress-bar') as HTMLElement).style.width = '0%';
  document.getElementById('wipe-progress-label')!.textContent = '0%';
  document.getElementById('wipe-btn-row')!.innerHTML =
    '<button id="btn-wipe-cancel">Cancel</button>';
  document.getElementById('btn-wipe-cancel')!.addEventListener('click', () =>
    invoke('cancel_wipe'),
  );
  if (wipePollId) clearInterval(wipePollId);
  wipePollId = setInterval(pollWipe, 500);
}

async function pollWipe(): Promise<void> {
  const p = await invoke<WipeProgress>('get_wipe_progress');
  updateWipeUI(p);
  if (p.finished) {
    clearInterval(wipePollId!);
    wipePollId = null;
  }
}

function updateWipeUI(p: WipeProgress): void {
  const banner = document.getElementById('wipe-status-banner')!;
  const bar = document.getElementById('wipe-progress-bar') as HTMLElement;
  const label = document.getElementById('wipe-progress-label')!;
  const sizeInfo = document.getElementById('wipe-size-info')!;

  if (p.error) {
    banner.className = 'banner danger';
    banner.textContent = 'Error: ' + p.error;
    banner.style.display = '';
  } else if (p.cancelled) {
    banner.className = 'banner warning';
    banner.textContent = 'Wipe cancelled. Free space was partially overwritten.';
    banner.style.display = '';
  } else if (p.finished) {
    banner.className = 'banner success';
    banner.textContent = 'Done — free space has been zeroed.';
    banner.style.display = '';
  }

  const frac = p.total_bytes > 0 ? Math.min(p.bytes_written / p.total_bytes, 1) : 0;
  const pct = Math.round(frac * 100);
  bar.style.width = `${pct}%`;
  label.textContent = `${pct}%`;

  if (p.total_bytes > 0) {
    sizeInfo.textContent = `${formatBytes(p.bytes_written)} of ${formatBytes(p.total_bytes)}`;
  } else if (!p.finished) {
    sizeInfo.textContent = 'Measuring available space…';
  }

  if (p.finished) {
    const btnRow = document.getElementById('wipe-btn-row')!;
    btnRow.innerHTML = '';
    const backBtn = document.createElement('button');
    backBtn.textContent = '← Back to Config';
    backBtn.addEventListener('click', () => showScreen('config'));
    btnRow.appendChild(backBtn);
  }
}

function formatBytes(bytes: number): string {
  if (bytes >= 1e12) return (bytes / 1e12).toFixed(1) + ' TB';
  if (bytes >= 1e9) return (bytes / 1e9).toFixed(1) + ' GB';
  if (bytes >= 1e6) return (bytes / 1e6).toFixed(1) + ' MB';
  if (bytes >= 1e3) return (bytes / 1e3).toFixed(1) + ' KB';
  return bytes + ' B';
}

// ── Utility ───────────────────────────────────────────────────────────────────

function parseSnapshotDate(name: string): Date | null {
  const m = name.match(/^(\d{4})-(\d{2})-(\d{2})T(\d{2})-(\d{2})-(\d{2})$/);
  if (!m) return null;
  return new Date(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], +m[6]);
}

function formatSnapshotDate(date: Date): string {
  const months = ['Jan','Feb','Mar','Apr','May','Jun','Jul','Aug','Sep','Oct','Nov','Dec'];
  const mon = months[date.getMonth()];
  const day = date.getDate();
  const hh = date.getHours().toString().padStart(2, '0');
  const mm = date.getMinutes().toString().padStart(2, '0');
  const yearSuffix = date.getFullYear() !== new Date().getFullYear()
    ? ` ${date.getFullYear()}` : '';
  return `${mon} ${day}${yearSuffix} at ${hh}:${mm}`;
}

function relativeTime(date: Date): string {
  const diffMs = Date.now() - date.getTime();
  const mins = Math.floor(diffMs / 60000);
  const hours = Math.floor(mins / 60);
  const days = Math.floor(hours / 24);
  const months = Math.floor(days / 30);
  if (months > 0) return `${months} month${months === 1 ? '' : 's'} ago`;
  if (days > 0)   return `${days} day${days === 1 ? '' : 's'} ago`;
  if (hours > 0)  return `${hours} hour${hours === 1 ? '' : 's'} ago`;
  if (mins > 0)   return `${mins} minute${mins === 1 ? '' : 's'} ago`;
  return 'just now';
}

function snapshotLabel(name: string): string {
  const date = parseSnapshotDate(name);
  if (!date) return escHtml(name);
  return `<strong>${escHtml(formatSnapshotDate(date))}</strong>`
    + ` <span class="radio-desc">${escHtml(relativeTime(date))} · ${escHtml(name)}</span>`;
}

function escHtml(s: unknown): string {
  return String(s ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

function setError(id: string, msg: string): void {
  const el = document.getElementById(id);
  if (!el) return;
  el.textContent = msg || '';
  el.style.display = msg ? '' : 'none';
}

// ── globals for inline onclick handlers ──────────────────────────────────────

window.toggleJob = toggleJob;
window.editJob = editJob;
window.togglePause = togglePause;
window.cancelBackup = cancelBackup;
window.showConfigFromBackup = showConfigFromBackup;
window.ejectDrive = ejectDrive;
window.goToRestore = goToRestore;

// ── Event wiring ──────────────────────────────────────────────────────────────

document.getElementById('btn-close-app')!.addEventListener('click', () => invoke('quit'));

document.getElementById('btn-refresh')!.addEventListener('click', refreshDrives);

document.getElementById('btn-open-drive')!.addEventListener('click', openSelectedDrive);

document.getElementById('btn-format-drive')!.addEventListener('click', () => {
  if (!selectedDevice) return;
  const drive = drives.find((d) => d.device === selectedDevice);
  if (!drive) return;
  enterFormatSetup(drive.device, drive.fstype, drive.dev_type === 'disk');
});

document.getElementById('btn-password-cancel')!.addEventListener('click', () => {
  showScreen('drive-select');
});
document.getElementById('btn-password-unlock')!.addEventListener('click', unlockDrive);
document.getElementById('password-input')!.addEventListener('keydown', (e: KeyboardEvent) => {
  if (e.key === 'Enter') unlockDrive();
});

document.getElementById('btn-eject')!.addEventListener('click', ejectDrive);
document.getElementById('btn-add-job')!.addEventListener('click', addJob);
document.getElementById('btn-save-config')!.addEventListener('click', saveConfig);
document.getElementById('btn-wipe-free-space')!.addEventListener('click', startWipe);
document.getElementById('btn-restore')!.addEventListener('click', goToRestore);
document.getElementById('btn-next')!.addEventListener('click', goToPreview);

document.getElementById('btn-job-cancel')!.addEventListener('click', async () => {
  const status = await invoke<AppStatus>('get_status');
  enterConfig(status.mount_point!, status.config!);
});
document.getElementById('btn-job-save')!.addEventListener('click', saveJob);
document.getElementById('btn-job-delete')!.addEventListener('click', deleteJob);
document.getElementById('job-source')!.addEventListener('blur', () => {
  const source = (document.getElementById('job-source') as HTMLInputElement).value.trim();
  if (source !== treeSource) {
    void reloadTreeSource(source);
  }
});
document.getElementById('job-excludes-manual')!.addEventListener('input', () => {
  renderExcludesTree();
  renderPresetChips();
});

document.getElementById('btn-restore-cancel')!.addEventListener('click', async () => {
  const status = await invoke<AppStatus>('get_status');
  enterConfig(status.mount_point!, status.config!);
});
document.getElementById('screen-restore')!.addEventListener('input', validateRestore);
document.getElementById('btn-do-restore')!.addEventListener('click', goToRestorePreview);

document.getElementById('btn-restore-preview-back')!.addEventListener('click', () => showScreen('restore'));
document.getElementById('btn-restore-preview-proceed')!.addEventListener('click', doRestore);

document.getElementById('btn-preview-cancel')!.addEventListener('click', async () => {
  const status = await invoke<AppStatus>('get_status');
  enterConfig(status.mount_point!, status.config!);
});
document.getElementById('btn-run-backup')!.addEventListener('click', startBackup);

document.getElementById('btn-format-cancel')!.addEventListener('click', () => {
  showScreen('drive-select');
});

['format-label', 'format-pass1', 'format-pass2', 'format-confirm-input'].forEach((id) => {
  document.getElementById(id)!.addEventListener('input', validateFormat);
});
document.getElementById('format-label')!.addEventListener('input', () => {
  if (formatDevice) buildFormatCmdPreview(formatDevice, formatIsDisk);
});
document.getElementById('format-fstype')!.addEventListener('change', () => {
  if (formatDevice) buildFormatCmdPreview(formatDevice, formatIsDisk);
});
document.getElementById('format-luks')!.addEventListener('change', () => {
  const luks = (document.getElementById('format-luks') as HTMLInputElement).checked;
  document.getElementById('format-luks-section')!.style.display = luks ? 'contents' : 'none';
  validateFormat();
  if (formatDevice) buildFormatCmdPreview(formatDevice, formatIsDisk);
});

document.getElementById('btn-do-format')!.addEventListener('click', doFormat);
document.getElementById('btn-format-done')!.addEventListener('click', async () => {
  await refreshDrives();
  showScreen('drive-select');
});

// ── Zoom ──────────────────────────────────────────────────────────────────────

const ZOOM_STEP = 0.1;
const ZOOM_MIN = 0.5;
const ZOOM_MAX = 3.0;
const ZOOM_DEFAULT = 1.5;

let zoomLevel: number = parseFloat(localStorage.getItem('zoom') ?? String(ZOOM_DEFAULT));

function applyZoom() {
  document.documentElement.style.zoom = String(zoomLevel);
  localStorage.setItem('zoom', String(zoomLevel));
}

document.addEventListener('keydown', (e) => {
  if (!e.ctrlKey) return;
  if (e.key === '=' || e.key === '+') {
    e.preventDefault();
    zoomLevel = Math.min(ZOOM_MAX, Math.round((zoomLevel + ZOOM_STEP) * 100) / 100);
    applyZoom();
  } else if (e.key === '-') {
    e.preventDefault();
    zoomLevel = Math.max(ZOOM_MIN, Math.round((zoomLevel - ZOOM_STEP) * 100) / 100);
    applyZoom();
  } else if (e.key === '0') {
    e.preventDefault();
    zoomLevel = ZOOM_DEFAULT;
    applyZoom();
  }
});

// ── Init ──────────────────────────────────────────────────────────────────────

applyZoom();
showScreen('drive-select');
refreshDrives();
