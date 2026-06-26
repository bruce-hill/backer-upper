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

// ── State ─────────────────────────────────────────────────────────────────────

let drives: DriveInfo[] = [];
let selectedDevice: string | null = null;
let editingJobIdx: number | null = null;
let backupPollId: ReturnType<typeof setInterval> | null = null;
let formatPollId: ReturnType<typeof setInterval> | null = null;
let probePollId: ReturnType<typeof setInterval> | null = null;
let formatDevice: string | null = null;
let formatIsDisk = false;
let operationIsRestore = false;

// ── Screen routing ────────────────────────────────────────────────────────────

function showScreen(name: string): void {
  document.querySelectorAll('.screen').forEach((s) => s.classList.remove('active'));
  document.getElementById('screen-' + name)!.classList.add('active');
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
      <td class="mono">${escHtml(j.source)}</td>
      <td>${escHtml(j.mode)}</td>
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
  (document.getElementById('job-excludes') as HTMLTextAreaElement).value = (
    job?.excludes || []
  ).join('\n');
  (document.getElementById('job-enabled') as HTMLInputElement).checked = job?.enabled ?? true;
  const mode = job?.mode || 'Backup';
  document.querySelectorAll<HTMLInputElement>('input[name="job-mode"]').forEach((r) => {
    r.checked = r.value === mode;
  });
  (document.getElementById('btn-job-delete') as HTMLElement).style.display =
    idx < jobs.length ? '' : 'none';
  showScreen('job-edit');
}

async function saveJob(): Promise<void> {
  const status = await invoke<AppStatus>('get_status');
  const config = status.config;
  if (!config) return;

  const excludes = (document.getElementById('job-excludes') as HTMLTextAreaElement).value
    .split('\n')
    .map((s) => s.trim())
    .filter(Boolean);

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
  const newStatus = await invoke<AppStatus>('get_status');
  enterConfig(newStatus.mount_point!, newStatus.config!);
}

async function deleteJob(): Promise<void> {
  if (editingJobIdx === null) return;
  try {
    await invoke<BackupConfig>('delete_job', { idx: editingJobIdx });
    const status = await invoke<AppStatus>('get_status');
    if (!status.mount_point || !status.config) { showScreen('drive-select'); return; }
    enterConfig(status.mount_point, status.config);
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
    btnRow.innerHTML = `
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
  const snapshotOptions = [
    { value: '', label: '<strong>Current backup</strong> — most recent rsync run' },
    ...snapshots.slice().reverse().map((s) => ({ value: s, label: escHtml(s) })),
  ];
  snapshotEl.innerHTML = snapshotOptions
    .map(
      (opt, i) => `
    <label class="radio-option">
      <input type="radio" name="restore-snapshot" value="${escHtml(opt.value)}" ${i === 0 ? 'checked' : ''} />
      <span>${opt.label}</span>
    </label>`
    )
    .join('');

  const jobsEl = document.getElementById('restore-jobs-list')!;
  if (jobs.length === 0) {
    jobsEl.innerHTML = '<p class="muted">No jobs configured.</p>';
  } else {
    jobsEl.innerHTML = jobs
      .map(
        (j, i) => `
      <label class="radio-option">
        <input type="checkbox" class="restore-job-check" data-idx="${i}" checked />
        <span>
          <strong>${escHtml(j.name)}</strong>
          <span class="radio-desc">${escHtml(String(j.destination))} → ${escHtml(j.source)}</span>
        </span>
      </label>`
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
    confirm === 'RESTORE' && hasJobs
  );
}

async function doRestore(): Promise<void> {
  const snapshotInput = document.querySelector<HTMLInputElement>(
    'input[name="restore-snapshot"]:checked'
  );
  const snapshotVal = snapshotInput?.value ?? '';
  const snapshot = snapshotVal === '' ? null : snapshotVal;

  const jobIndices = Array.from(
    document.querySelectorAll<HTMLInputElement>('.restore-job-check')
  )
    .filter((cb) => cb.checked)
    .map((cb) => parseInt(cb.dataset.idx!, 10));

  const deleteExtra = (document.getElementById('restore-delete-extra') as HTMLInputElement).checked;

  setError('restore-error', '');
  try {
    await invoke('start_restore', { snapshot, jobIndices, deleteExtra });
  } catch (e) {
    setError('restore-error', String(e));
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
  (document.getElementById('format-pass1') as HTMLInputElement).value = '';
  (document.getElementById('format-pass2') as HTMLInputElement).value = '';
  (document.getElementById('format-confirm-input') as HTMLInputElement).value = '';
  setError('format-error', '');
  setError('format-validation-msg', '');
  (document.getElementById('btn-do-format') as HTMLButtonElement).disabled = true;

  if (drive && drive.is_mounted) {
    (document.getElementById('format-mounted-error') as HTMLElement).style.display = '';
    (document.getElementById('format-main-content') as HTMLElement).style.display = 'none';
  } else {
    (document.getElementById('format-mounted-error') as HTMLElement).style.display = 'none';
    (document.getElementById('format-main-content') as HTMLElement).style.display = '';
    buildFormatCmdPreview(device, isDisk);
    startProbe(device, fstype);
  }

  showScreen('format-setup');
}

function buildFormatCmdPreview(device: string, isDisk: boolean): void {
  const label =
    (document.getElementById('format-label') as HTMLInputElement).value.trim() || '<label>';
  const part = isDisk ? (device.match(/\d$/) ? device + 'p1' : device + '1') : device;
  const lines: string[] = [];
  if (isDisk) {
    lines.push('doas wipefs -a ' + device);
    lines.push('doas parted -s ' + device + ' mklabel gpt mkpart primary 0% 100%');
  } else {
    lines.push('doas wipefs -a ' + device);
  }
  lines.push('doas cryptsetup luksFormat --type luks2 ' + part);
  lines.push('doas cryptsetup luksOpen ' + part + ' backer-upper-format');
  lines.push('doas mkfs.btrfs -L ' + label + ' /dev/mapper/backer-upper-format');
  lines.push('doas cryptsetup luksClose backer-upper-format');
  document.getElementById('format-cmd-preview')!.textContent = lines.join('\n');
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
  const p1 = (document.getElementById('format-pass1') as HTMLInputElement).value;
  const p2 = (document.getElementById('format-pass2') as HTMLInputElement).value;
  const confirm = (document.getElementById('format-confirm-input') as HTMLInputElement).value.trim();

  let msg = '';
  if (p2 && p1 !== p2) msg = 'Passphrases do not match.';
  if (confirm && confirm !== formatDevice) msg = 'Must match exactly: ' + formatDevice;

  const msgEl = document.getElementById('format-validation-msg')!;
  if (msg) {
    msgEl.textContent = msg;
    msgEl.style.display = '';
  } else {
    msgEl.textContent = '';
    msgEl.style.display = 'none';
  }

  const ok = label && p1 && p1 === p2 && confirm === formatDevice;
  (document.getElementById('btn-do-format') as HTMLButtonElement).disabled = !ok;
}

async function doFormat(): Promise<void> {
  const label = (document.getElementById('format-label') as HTMLInputElement).value.trim();
  const passphrase = (document.getElementById('format-pass1') as HTMLInputElement).value;
  (document.getElementById('format-pass1') as HTMLInputElement).value = '';
  (document.getElementById('format-pass2') as HTMLInputElement).value = '';
  (document.getElementById('format-confirm-input') as HTMLInputElement).value = '';

  try {
    await invoke('start_format', {
      device: formatDevice,
      isDisk: formatIsDisk,
      label,
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
      if (n < p.step) cls += ' done';
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

// ── Utility ───────────────────────────────────────────────────────────────────

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
document.getElementById('btn-restore')!.addEventListener('click', goToRestore);
document.getElementById('btn-next')!.addEventListener('click', goToPreview);

document.getElementById('btn-job-cancel')!.addEventListener('click', async () => {
  const status = await invoke<AppStatus>('get_status');
  enterConfig(status.mount_point!, status.config!);
});
document.getElementById('btn-job-save')!.addEventListener('click', saveJob);
document.getElementById('btn-job-delete')!.addEventListener('click', deleteJob);

document.getElementById('btn-restore-cancel')!.addEventListener('click', async () => {
  const status = await invoke<AppStatus>('get_status');
  enterConfig(status.mount_point!, status.config!);
});
document.getElementById('screen-restore')!.addEventListener('input', validateRestore);
document.getElementById('btn-do-restore')!.addEventListener('click', doRestore);

document.getElementById('btn-preview-cancel')!.addEventListener('click', async () => {
  const status = await invoke<AppStatus>('get_status');
  enterConfig(status.mount_point!, status.config!);
});
document.getElementById('btn-run-backup')!.addEventListener('click', startBackup);

document.getElementById('btn-format-cancel')!.addEventListener('click', () => {
  showScreen('drive-select');
});
document.getElementById('btn-format-mounted-back')!.addEventListener('click', () => {
  showScreen('drive-select');
});
document.getElementById('btn-format-eject')!.addEventListener('click', async (e) => {
  const btn = e.currentTarget as HTMLButtonElement;
  btn.disabled = true;
  setStatusBar('Ejecting…', true);
  const drive = drives.find((d) => d.device === selectedDevice);
  if (!drive) { btn.disabled = false; setStatusBar(''); return; }
  try {
    await invoke('eject');
  } catch (_) {}
  await refreshDrives();
  setStatusBar('');
  const updated = drives.find((d) => d.device === (drive.luks_parent || drive.device));
  if (updated) {
    selectedDevice = updated.device;
    enterFormatSetup(updated.device, updated.fstype, updated.dev_type === 'disk');
  } else {
    showScreen('drive-select');
  }
});

['format-label', 'format-pass1', 'format-pass2', 'format-confirm-input'].forEach((id) => {
  document.getElementById(id)!.addEventListener('input', validateFormat);
});
document.getElementById('format-label')!.addEventListener('input', () => {
  if (formatDevice) buildFormatCmdPreview(formatDevice, formatIsDisk);
});

document.getElementById('btn-do-format')!.addEventListener('click', doFormat);
document.getElementById('btn-format-done')!.addEventListener('click', async () => {
  await refreshDrives();
  showScreen('drive-select');
});

// ── Init ──────────────────────────────────────────────────────────────────────

showScreen('drive-select');
refreshDrives();
