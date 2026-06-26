const { invoke } = window.__TAURI__.core;

// ── State ─────────────────────────────────────────────────────────────────────

let drives = [];
let selectedDevice = null;
let currentScreen = null;
let editingJobIdx = null;
let backupPollId = null;
let formatPollId = null;
let probePollId = null;
let formatDevice = null;
let formatIsDisk = false;

// ── Screen routing ────────────────────────────────────────────────────────────

function showScreen(name) {
  document.querySelectorAll('.screen').forEach((s) => s.classList.remove('active'));
  document.getElementById('screen-' + name).classList.add('active');
  currentScreen = name;
}

function setStatusBar(msg) {
  document.getElementById('status-bar').textContent = msg || '';
}

// ── Drive Select screen ───────────────────────────────────────────────────────

async function refreshDrives() {
  try {
    drives = await invoke('list_drives');
  } catch (e) {
    drives = [];
    setStatusBar('Error listing drives: ' + e);
  }
  renderDriveList();
}

function renderDriveList() {
  const el = document.getElementById('drive-list');
  if (drives.length === 0) {
    el.innerHTML = '<div class="empty-state">No removable drives detected.</div>';
  } else {
    el.innerHTML = drives
      .map((d, i) => {
        const badges = [];
        if (d.tran) badges.push(`<span class="badge badge-usb">${d.tran.toUpperCase()}</span>`);
        if (d.fstype && d.fstype !== 'crypto_LUKS')
          badges.push(`<span class="badge badge-fs">${d.fstype}</span>`);
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
      item.addEventListener('click', () => {
        selectedDevice = item.dataset.device;
        renderDriveList();
        updateDriveSelectButtons();
      });
      item.addEventListener('dblclick', () => openSelectedDrive());
    });
  }
  updateDriveSelectButtons();
}

function updateDriveSelectButtons() {
  const drive = drives.find((d) => d.device === selectedDevice);
  document.getElementById('btn-open-drive').disabled = !drive;
  const canFormat = drive && (drive.dev_type === 'disk' || drive.dev_type === 'part');
  document.getElementById('btn-format-drive').disabled = !canFormat;
}

async function openSelectedDrive() {
  if (!selectedDevice) return;
  setError('drive-select-error', '');
  try {
    const result = await invoke('open_drive', { device: selectedDevice });
    if (result.error) {
      setError('drive-select-error', result.error);
    } else if (result.needs_password) {
      const drive = drives.find((d) => d.device === selectedDevice);
      document.getElementById('password-heading').textContent =
        'Unlock: ' + (drive ? drive.display_name : selectedDevice);
      document.getElementById('password-input').value = '';
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

async function unlockDrive() {
  const pw = document.getElementById('password-input').value;
  setError('password-error', '');
  document.getElementById('btn-password-unlock').disabled = true;
  try {
    const result = await invoke('unlock_drive', { device: selectedDevice, password: pw });
    enterConfig(result.mount_point, result.config);
  } catch (e) {
    setError('password-error', String(e));
  } finally {
    document.getElementById('btn-password-unlock').disabled = false;
  }
}

// ── Config Editor screen ──────────────────────────────────────────────────────

function enterConfig(mountPoint, config) {
  renderConfig(mountPoint, config);
  showScreen('config');
}

function renderConfig(mountPoint, config) {
  document.getElementById('config-mount-point').textContent = 'Drive: ' + mountPoint;
  if (config.last_backup) {
    const d = new Date(config.last_backup);
    document.getElementById('config-last-backup').textContent =
      'Last backup: ' + d.toLocaleString();
  } else {
    document.getElementById('config-last-backup').textContent = '';
  }
  renderJobsTable(config.jobs);
}

function renderJobsTable(jobs) {
  const tbody = document.getElementById('jobs-tbody');
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

async function toggleJob(idx, enabled) {
  const status = await invoke('get_status');
  if (!status.config) return;
  const config = status.config;
  config.jobs[idx].enabled = enabled;
  await invoke('update_config', { config });
  await refreshConfigView();
}

async function refreshConfigView() {
  const status = await invoke('get_status');
  if (status.config) {
    renderJobsTable(status.config.jobs);
    const dirty = status.config_dirty;
    document.getElementById('btn-save-config').style.display = dirty ? '' : 'none';
    document.getElementById('unsaved-indicator').style.display = dirty ? '' : 'none';
  }
}

async function addJob() {
  try {
    const config = await invoke('add_job');
    editJob(config.jobs.length - 1);
  } catch (e) {
    setError('config-error', String(e));
  }
}

async function editJob(idx) {
  const status = await invoke('get_status');
  const jobs = status.config?.jobs || [];
  const job = jobs[idx];
  editingJobIdx = idx;

  document.getElementById('job-edit-heading').textContent =
    idx < jobs.length ? 'Edit Job' : 'New Job';
  document.getElementById('job-name').value = job?.name || '';
  document.getElementById('job-source').value = job?.source || '';
  document.getElementById('job-dest').value = job?.destination || '';
  document.getElementById('job-excludes').value = (job?.excludes || []).join('\n');
  document.getElementById('job-enabled').checked = job?.enabled ?? true;
  const mode = job?.mode || 'Backup';
  document.querySelectorAll('input[name="job-mode"]').forEach((r) => {
    r.checked = r.value === mode;
  });
  document.getElementById('btn-job-delete').style.display = idx < jobs.length ? '' : 'none';
  showScreen('job-edit');
}

async function saveJob() {
  const status = await invoke('get_status');
  const config = status.config;
  if (!config) return;

  const excludes = document
    .getElementById('job-excludes')
    .value.split('\n')
    .map((s) => s.trim())
    .filter(Boolean);

  const modeEl = document.querySelector('input[name="job-mode"]:checked');
  const job = {
    name: document.getElementById('job-name').value,
    source: document.getElementById('job-source').value,
    destination: document.getElementById('job-dest').value,
    excludes,
    mode: modeEl ? modeEl.value : 'Backup',
    enabled: document.getElementById('job-enabled').checked,
  };

  if (editingJobIdx < config.jobs.length) {
    config.jobs[editingJobIdx] = job;
  } else {
    config.jobs.push(job);
  }

  await invoke('update_config', { config });
  const newStatus = await invoke('get_status');
  enterConfig(newStatus.mount_point, newStatus.config);
}

async function deleteJob() {
  if (editingJobIdx === null) return;
  try {
    const config = await invoke('delete_job', { idx: editingJobIdx });
    const status = await invoke('get_status');
    enterConfig(status.mount_point, status.config);
  } catch (e) {
    alert('Delete failed: ' + e);
  }
}

async function saveConfig() {
  try {
    await invoke('save_config');
    document.getElementById('btn-save-config').style.display = 'none';
    document.getElementById('unsaved-indicator').style.display = 'none';
    setStatusBar('Config saved.');
    setTimeout(() => setStatusBar(''), 2000);
  } catch (e) {
    setError('config-error', String(e));
  }
}

async function ejectDrive() {
  try {
    await invoke('eject');
    selectedDevice = null;
    await refreshDrives();
    showScreen('drive-select');
  } catch (e) {
    setError('config-error', String(e));
  }
}

async function goToPreview() {
  const status = await invoke('get_status');
  if (status.config_dirty) {
    await invoke('save_config');
  }
  const cmds = await invoke('preview_commands');
  const el = document.getElementById('preview-commands');
  const empty = document.getElementById('preview-empty');
  if (cmds.length === 0) {
    el.innerHTML = '';
    empty.style.display = '';
    document.getElementById('btn-run-backup').disabled = true;
  } else {
    empty.style.display = 'none';
    el.innerHTML = cmds
      .map(
        (c) => `
      <div class="preview-cmd-block">
        <div class="cmd-name">${escHtml(c.name)}</div>
        <pre>${escHtml(c.cmd)}</pre>
      </div>`
      )
      .join('');
    document.getElementById('btn-run-backup').disabled = false;
  }
  showScreen('preview');
}

// ── Backup screen ─────────────────────────────────────────────────────────────

async function startBackup() {
  try {
    await invoke('start_backup');
  } catch (e) {
    alert('Failed to start backup: ' + e);
    return;
  }
  showScreen('backup');
  resetBackupUI();
  startBackupPoll();
}

function resetBackupUI() {
  document.getElementById('backup-status-banner').style.display = 'none';
  document.getElementById('backup-job-info').textContent = '';
  setProgressBar(0, false, false);
  document.getElementById('backup-elapsed').textContent = '';
  document.getElementById('backup-log').innerHTML = '';
  const btnRow = document.getElementById('backup-btn-row');
  btnRow.innerHTML = `
    <button id="btn-backup-pause" onclick="togglePause()">Pause</button>
    <button id="btn-backup-cancel" onclick="cancelBackup()">Cancel</button>`;
}

let _paused = false;

async function togglePause() {
  if (_paused) {
    await invoke('resume_backup');
    _paused = false;
    document.getElementById('btn-backup-pause').textContent = 'Pause';
  } else {
    await invoke('pause_backup');
    _paused = true;
    document.getElementById('btn-backup-pause').textContent = 'Resume';
  }
}

async function cancelBackup() {
  await invoke('cancel_backup');
}

function startBackupPoll() {
  if (backupPollId) clearInterval(backupPollId);
  backupPollId = setInterval(pollBackup, 250);
}

async function pollBackup() {
  const p = await invoke('get_backup_progress');
  updateBackupUI(p);
  if (!p.running && (p.finished || p.error)) {
    clearInterval(backupPollId);
    backupPollId = null;
  }
}

function updateBackupUI(p) {
  const banner = document.getElementById('backup-status-banner');
  const jobInfo = document.getElementById('backup-job-info');
  const elapsed = document.getElementById('backup-elapsed');

  const pct = Math.round(p.overall_fraction * 100);
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
      ? `<span style="font-weight:600">${escHtml(p.job_name)}</span>
         <span class="muted" style="margin-left:8px">${escHtml(p.current_file)}</span>
         <span class="muted" style="margin-left:8px">ETA: ${escHtml(p.eta)}</span>`
      : '';
  }

  // Log
  const logEl = document.getElementById('backup-log');
  const wasAtBottom = logEl.scrollHeight - logEl.clientHeight <= logEl.scrollTop + 4;
  logEl.innerHTML = p.log_lines
    .map((l) => `<div class="${l.startsWith('>>>') ? 'log-cmd' : ''}">${escHtml(l)}</div>`)
    .join('');
  if (wasAtBottom) logEl.scrollTop = logEl.scrollHeight;

  // Swap buttons when done
  if (!p.running && (p.finished || p.error)) {
    const btnRow = document.getElementById('backup-btn-row');
    btnRow.innerHTML = `
      <button onclick="showConfigFromBackup()">← Back to Config</button>
      <button onclick="ejectFromBackup()" class="primary">Eject Drive</button>`;
  }
}

function setProgressBar(fraction, error, done) {
  const bar = document.getElementById('backup-progress-bar');
  const label = document.getElementById('backup-progress-label');
  const pct = Math.round(fraction * 100);
  bar.style.width = pct + '%';
  label.textContent = pct + '%';
  bar.className = 'progress-bar' + (error ? ' error' : done ? ' done' : '');
}

async function showConfigFromBackup() {
  const status = await invoke('get_status');
  enterConfig(status.mount_point, status.config);
}

async function ejectFromBackup() {
  await ejectDrive();
}

// ── Format Setup screen ───────────────────────────────────────────────────────

async function enterFormatSetup(device, fstype, isDisk) {
  formatDevice = device;
  formatIsDisk = isDisk;

  const drive = drives.find((d) => d.device === device);
  const infoLine = drive
    ? `${drive.device} · ${drive.display_name} · ${drive.size || 'unknown size'}`
    : device;
  document.getElementById('format-drive-info-line').textContent = infoLine;
  document.getElementById('format-confirm-label').textContent = `Type ${device} to confirm`;
  document.getElementById('format-confirm-input').placeholder = device;

  document.getElementById('format-label').value = 'Backup';
  document.getElementById('format-pass1').value = '';
  document.getElementById('format-pass2').value = '';
  document.getElementById('format-confirm-input').value = '';
  setError('format-error', '');
  setError('format-validation-msg', '');
  document.getElementById('btn-do-format').disabled = true;

  if (drive && drive.is_mounted) {
    document.getElementById('format-mounted-error').style.display = '';
    document.getElementById('format-main-content').style.display = 'none';
  } else {
    document.getElementById('format-mounted-error').style.display = 'none';
    document.getElementById('format-main-content').style.display = '';
    buildFormatCmdPreview(device, isDisk);
    startProbe(device, fstype);
  }

  showScreen('format-setup');
}

function buildFormatCmdPreview(device, isDisk) {
  const label = document.getElementById('format-label').value.trim() || '<label>';
  const part = isDisk ? (device.match(/\d$/) ? device + 'p1' : device + '1') : device;
  const lines = [];
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
  document.getElementById('format-cmd-preview').textContent = lines.join('\n');
}

function startProbe(device, fstype) {
  document.getElementById('format-probe-content').innerHTML =
    '<em style="color:#9ca3af">Reading drive contents…</em>';
  invoke('start_probe_drive', { device, fstype: fstype || null });
  if (probePollId) clearInterval(probePollId);
  probePollId = setInterval(pollProbe, 300);
}

async function pollProbe() {
  const info = await invoke('get_drive_probe');
  if (!info.finished) return;
  clearInterval(probePollId);
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
  document.getElementById('format-probe-content').innerHTML = html || '(no info)';
}

function validateFormat() {
  const label = document.getElementById('format-label').value.trim();
  const p1 = document.getElementById('format-pass1').value;
  const p2 = document.getElementById('format-pass2').value;
  const confirm = document.getElementById('format-confirm-input').value.trim();

  let msg = '';
  if (p2 && p1 !== p2) msg = 'Passphrases do not match.';
  if (confirm && confirm !== formatDevice) msg = 'Must match exactly: ' + formatDevice;

  const msgEl = document.getElementById('format-validation-msg');
  if (msg) {
    msgEl.textContent = msg;
    msgEl.style.display = '';
  } else {
    msgEl.textContent = '';
    msgEl.style.display = 'none';
  }

  const ok = label && p1 && p1 === p2 && confirm === formatDevice;
  document.getElementById('btn-do-format').disabled = !ok;
}

async function doFormat() {
  const label = document.getElementById('format-label').value.trim();
  const passphrase = document.getElementById('format-pass1').value;
  document.getElementById('format-pass1').value = '';
  document.getElementById('format-pass2').value = '';
  document.getElementById('format-confirm-input').value = '';

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
  document.getElementById('format-progress-banner').style.display = 'none';
  document.getElementById('format-step-label').textContent = 'Starting…';
  document.getElementById('format-step-dots').innerHTML = '';
  document.getElementById('format-log').innerHTML = '';
  document.getElementById('format-done-btn-row').style.display = 'none';

  if (formatPollId) clearInterval(formatPollId);
  formatPollId = setInterval(pollFormat, 300);
}

async function pollFormat() {
  const p = await invoke('get_format_progress');
  updateFormatUI(p);
  if (p.finished) {
    clearInterval(formatPollId);
    formatPollId = null;
  }
}

function updateFormatUI(p) {
  const banner = document.getElementById('format-progress-banner');
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
    document.getElementById('format-step-label').textContent =
      `Step ${p.step} of ${p.total_steps}: ${p.step_name}`;
  } else {
    document.getElementById('format-step-label').textContent = '';
  }

  const dotsEl = document.getElementById('format-step-dots');
  if (p.total_steps > 0) {
    dotsEl.innerHTML = Array.from({ length: p.total_steps }, (_, i) => {
      const n = i + 1;
      let cls = 'step-dot';
      if (n < p.step) cls += ' done';
      else if (n === p.step) cls += p.error ? ' error' : ' active';
      return `<div class="${cls}">${n}</div>`;
    }).join('');
  }

  const logEl = document.getElementById('format-log');
  const wasAtBottom = logEl.scrollHeight - logEl.clientHeight <= logEl.scrollTop + 4;
  logEl.innerHTML = p.log
    .map((l) => `<div class="${l.startsWith('>>>') ? 'log-cmd' : ''}">${escHtml(l)}</div>`)
    .join('');
  if (wasAtBottom) logEl.scrollTop = logEl.scrollHeight;

  if (p.finished) {
    document.getElementById('format-done-btn-row').style.display = '';
  }
}

// ── Utility ───────────────────────────────────────────────────────────────────

function escHtml(s) {
  return String(s ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

function setError(id, msg) {
  const el = document.getElementById(id);
  if (!el) return;
  el.textContent = msg || '';
  el.style.display = msg ? '' : 'none';
}

// ── Event wiring ──────────────────────────────────────────────────────────────

document.getElementById('btn-refresh').addEventListener('click', refreshDrives);

document.getElementById('btn-open-drive').addEventListener('click', openSelectedDrive);

document.getElementById('btn-format-drive').addEventListener('click', () => {
  if (!selectedDevice) return;
  const drive = drives.find((d) => d.device === selectedDevice);
  if (!drive) return;
  enterFormatSetup(drive.device, drive.fstype, drive.dev_type === 'disk');
});

document.getElementById('btn-password-cancel').addEventListener('click', () => {
  showScreen('drive-select');
});
document.getElementById('btn-password-unlock').addEventListener('click', unlockDrive);
document.getElementById('password-input').addEventListener('keydown', (e) => {
  if (e.key === 'Enter') unlockDrive();
});

document.getElementById('btn-eject').addEventListener('click', ejectDrive);
document.getElementById('btn-add-job').addEventListener('click', addJob);
document.getElementById('btn-save-config').addEventListener('click', saveConfig);
document.getElementById('btn-next').addEventListener('click', goToPreview);

document.getElementById('btn-job-cancel').addEventListener('click', async () => {
  const status = await invoke('get_status');
  enterConfig(status.mount_point, status.config);
});
document.getElementById('btn-job-save').addEventListener('click', saveJob);
document.getElementById('btn-job-delete').addEventListener('click', deleteJob);

document.getElementById('btn-preview-cancel').addEventListener('click', async () => {
  const status = await invoke('get_status');
  enterConfig(status.mount_point, status.config);
});
document.getElementById('btn-run-backup').addEventListener('click', startBackup);

document.getElementById('btn-format-cancel').addEventListener('click', () => {
  showScreen('drive-select');
});
document.getElementById('btn-format-mounted-back').addEventListener('click', () => {
  showScreen('drive-select');
});
document.getElementById('btn-format-eject').addEventListener('click', async () => {
  const drive = drives.find((d) => d.device === selectedDevice);
  if (!drive) return;
  try {
    await invoke('eject');
  } catch (_) {}
  await refreshDrives();
  const updated = drives.find((d) => d.device === (drive.luks_parent || drive.device));
  if (updated) {
    selectedDevice = updated.device;
    enterFormatSetup(updated.device, updated.fstype, updated.dev_type === 'disk');
  } else {
    showScreen('drive-select');
  }
});

['format-label', 'format-pass1', 'format-pass2', 'format-confirm-input'].forEach((id) => {
  document.getElementById(id).addEventListener('input', validateFormat);
});
document.getElementById('format-label').addEventListener('input', () => {
  if (formatDevice) buildFormatCmdPreview(formatDevice, formatIsDisk);
});

document.getElementById('btn-do-format').addEventListener('click', doFormat);
document.getElementById('btn-format-done').addEventListener('click', async () => {
  await refreshDrives();
  showScreen('drive-select');
});

// ── Init ──────────────────────────────────────────────────────────────────────

showScreen('drive-select');
refreshDrives();
