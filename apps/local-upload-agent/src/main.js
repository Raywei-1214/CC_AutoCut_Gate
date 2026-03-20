const healthElement = document.querySelector('#status-health')
const versionElement = document.querySelector('#status-version')
const platformElement = document.querySelector('#status-platform')
const capabilitiesElement = document.querySelector('#status-capabilities')
const refreshButton = document.querySelector('#refresh-button')
const pickFileButton = document.querySelector('#pick-file-button')
const exportLogsButton = document.querySelector('#export-logs-button')
const checkUpdateButton = document.querySelector('#check-update-button')
const installUpdateButton = document.querySelector('#install-update-button')
const taskListElement = document.querySelector('#task-list')
const taskEmptyElement = document.querySelector('#task-empty')
const updateStatusElement = document.querySelector('#update-status')
const updateCurrentVersionElement = document.querySelector('#update-current-version')
const updateLatestVersionElement = document.querySelector('#update-latest-version')
const updateProgressElement = document.querySelector('#update-progress')
const updateNotesElement = document.querySelector('#update-notes')

const AGENT_BASE_URL = 'http://127.0.0.1:17777'
const HEALTH_URL = `${AGENT_BASE_URL}/v1/health`
const FILE_PICK_URL = `${AGENT_BASE_URL}/v1/files/pick`
const TASKS_URL = `${AGENT_BASE_URL}/v1/uploads`
const UPDATE_URL = `${AGENT_BASE_URL}/v1/system/update`

let latestTasks = []
let latestUpdateState = null

async function requestJson(url, init) {
  const response = await fetch(url, init)
  const payload = await response.json()

  if (!response.ok || !payload?.success) {
    throw new Error(payload?.error || `HTTP ${response.status}`)
  }

  return payload
}

function formatBytes(bytes) {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B'

  const units = ['B', 'KB', 'MB', 'GB']
  let value = bytes
  let unitIndex = 0

  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024
    unitIndex += 1
  }

  return `${value.toFixed(value >= 100 ? 0 : 1)} ${units[unitIndex]}`
}

function formatTaskStatus(status) {
  switch (status) {
    case 'queued':
      return '排队中'
    case 'uploading':
      return '上传中'
    case 'finalizing':
      return '保存中'
    case 'completed':
      return '已完成'
    case 'cancelled':
      return '已取消'
    case 'failed':
      return '失败'
    default:
      return status
  }
}

function formatUpdateStatus(updateState) {
  if (!updateState?.configured) return '未配置'
  if (updateState.installing) return '安装中'
  if (updateState.checking) return '检查中'
  if (updateState.available) return '发现新版本'
  if (updateState.lastError) return '检查失败'
  if (updateState.lastCheckedAt) return '已是最新'
  return '未检查'
}

function renderTasks(tasks) {
  if (!taskListElement || !taskEmptyElement) return

  latestTasks = tasks
  taskListElement.innerHTML = ''
  taskEmptyElement.hidden = tasks.length > 0

  for (const task of tasks) {
    const card = document.createElement('article')
    card.className = 'task-card'

    const header = document.createElement('div')
    header.className = 'task-card-header'
    header.innerHTML = `
      <div>
        <p class="task-file">${task.fileName}</p>
        <p class="task-meta">任务 ${task.taskId}</p>
      </div>
      <span class="task-status task-status-${task.status}">${formatTaskStatus(task.status)}</span>
    `

    const progress = document.createElement('div')
    progress.className = 'task-progress'
    progress.innerHTML = `
      <div class="task-progress-bar">
        <div class="task-progress-fill" style="width:${Math.max(0, Math.min(100, task.progress ?? 0))}%"></div>
      </div>
      <div class="task-stats">
        <span>${Math.round(task.progress ?? 0)}%</span>
        <span>${formatBytes(task.uploadedBytes ?? 0)} / ${formatBytes(task.totalBytes ?? task.fileSize ?? 0)}</span>
        <span>${formatBytes(task.speedBytesPerSecond ?? 0)}/s</span>
      </div>
    `

    const actions = document.createElement('div')
    actions.className = 'task-actions'

    const logButton = document.createElement('button')
    logButton.type = 'button'
    logButton.className = 'ghost-button'
    logButton.textContent = '查看日志'
    logButton.addEventListener('click', () => {
      void showTaskLogs(task.taskId, task.fileName)
    })
    actions.appendChild(logButton)

    if (task.status === 'queued' || task.status === 'uploading' || task.status === 'finalizing') {
      const cancelButton = document.createElement('button')
      cancelButton.type = 'button'
      cancelButton.className = 'danger-button'
      cancelButton.textContent = '取消'
      cancelButton.addEventListener('click', () => {
        void cancelTask(task.taskId)
      })
      actions.appendChild(cancelButton)
    }

    if (task.error) {
      const errorText = document.createElement('p')
      errorText.className = 'task-error'
      errorText.textContent = task.error
      card.append(header, progress, actions, errorText)
    } else {
      card.append(header, progress, actions)
    }

    taskListElement.appendChild(card)
  }
}

function renderUpdateState(updateState) {
  latestUpdateState = updateState
  if (!updateStatusElement) return

  updateStatusElement.textContent = formatUpdateStatus(updateState)
  if (updateCurrentVersionElement) {
    updateCurrentVersionElement.textContent = updateState?.currentVersion || '0.1.0'
  }
  if (updateLatestVersionElement) {
    updateLatestVersionElement.textContent = updateState?.latestVersion || '-'
  }
  if (updateProgressElement) {
    const totalBytes = Number(updateState?.downloadTotalBytes || 0)
    const downloadedBytes = Number(updateState?.downloadedBytes || 0)
    const progress =
      totalBytes > 0
        ? `${Math.round((downloadedBytes / totalBytes) * 100)}%`
        : `${Math.round(updateState?.progress || 0)}%`
    updateProgressElement.textContent = progress
  }
  if (updateNotesElement) {
    if (updateState?.lastError) {
      updateNotesElement.textContent = updateState.lastError
    } else if (updateState?.notes) {
      updateNotesElement.textContent = updateState.notes
    } else if (!updateState?.configured) {
      updateNotesElement.textContent = '当前构建未嵌入自动更新配置。'
    } else if (updateState?.available) {
      updateNotesElement.textContent = '检测到可安装更新。'
    } else {
      updateNotesElement.textContent = '当前未检测到更新。'
    }
  }

  if (checkUpdateButton) {
    checkUpdateButton.disabled =
      !updateState?.configured || Boolean(updateState?.checking || updateState?.installing)
  }
  if (installUpdateButton) {
    installUpdateButton.disabled =
      !updateState?.configured ||
      !updateState?.available ||
      Boolean(updateState?.checking || updateState?.installing)
  }
}

async function refreshTasks() {
  try {
    const payload = await requestJson(TASKS_URL, {
      method: 'GET',
    })

    renderTasks(Array.isArray(payload.data) ? payload.data : [])
  } catch {
    renderTasks(latestTasks)
  }
}

async function refreshUpdateState() {
  try {
    const payload = await requestJson(UPDATE_URL, {
      method: 'GET',
    })

    renderUpdateState(payload.data ?? null)
  } catch (error) {
    renderUpdateState({
      configured: false,
      checking: false,
      installing: false,
      available: false,
      currentVersion: latestUpdateState?.currentVersion || '0.1.0',
      latestVersion: null,
      progress: 0,
      downloadedBytes: 0,
      downloadTotalBytes: null,
      notes: null,
      lastError: error instanceof Error ? error.message : '读取更新状态失败',
      lastCheckedAt: latestUpdateState?.lastCheckedAt || null,
    })
  }
}

async function refreshHealth() {
  if (!healthElement || !versionElement || !platformElement || !capabilitiesElement) return

  healthElement.textContent = '检查中...'
  capabilitiesElement.textContent = '检测中...'

  try {
    const payload = await requestJson(HEALTH_URL, {
      method: 'GET',
    })

    healthElement.textContent = '已连接'
    versionElement.textContent = payload.data?.version || '0.1.0'
    platformElement.textContent = payload.data?.platform || 'unknown'
    capabilitiesElement.textContent = Array.isArray(payload.data?.capabilities)
      ? payload.data.capabilities.join(' / ')
      : 'unknown'
  } catch (error) {
    healthElement.textContent = error instanceof Error ? `未连接：${error.message}` : '未连接'
    platformElement.textContent = navigator.platform || 'unknown'
    capabilitiesElement.textContent = 'unknown'
  }
}

async function showTaskLogs(taskId, fileName) {
  const payload = await requestJson(`${TASKS_URL}/${taskId}/logs`, {
    method: 'GET',
  })

  const logs = Array.isArray(payload.data) ? payload.data : []
  const lines = logs.map((log) => {
    const chunkText = typeof log.chunkIndex === 'number' ? ` [chunk ${log.chunkIndex}]` : ''
    const detailText = log.detail ? ` · ${log.detail}` : ''
    return `${log.timestamp} [${log.level}]${chunkText} ${log.message}${detailText}`
  })

  window.alert(`${fileName} 日志\n\n${lines.join('\n') || '暂无日志'}`)
}

async function cancelTask(taskId) {
  await requestJson(`${TASKS_URL}/${taskId}/cancel`, {
    method: 'POST',
  })
  await refreshTasks()
}

async function pickFile() {
  const payload = await requestJson(FILE_PICK_URL, {
    method: 'POST',
  })

  const file = payload.data
  window.alert(
    `已选择文件：${file.fileName}\n路径：${file.localFilePath}\n大小：${formatBytes(file.fileSize)}\n\n接下来仍由网页端发起上传会话。`,
  )
}

async function exportLogs() {
  const sections = []

  for (const task of latestTasks) {
    let logLines = ['暂无日志']

    try {
      const payload = await requestJson(`${TASKS_URL}/${task.taskId}/logs`, {
        method: 'GET',
      })

      const logs = Array.isArray(payload.data) ? payload.data : []
      if (logs.length > 0) {
        logLines = logs.map((log) => {
          const chunkText = typeof log.chunkIndex === 'number' ? ` [chunk ${log.chunkIndex}]` : ''
          const detailText = log.detail ? ` · ${log.detail}` : ''
          return `${log.timestamp} [${log.level}]${chunkText} ${log.message}${detailText}`
        })
      }
    } catch (error) {
      logLines = [error instanceof Error ? error.message : '导出日志失败']
    }

    sections.push(
      [
        `任务: ${task.taskId}`,
        `文件: ${task.fileName}`,
        `状态: ${task.status}`,
        `进度: ${Math.round(task.progress ?? 0)}%`,
        `已传: ${task.uploadedBytes ?? 0}`,
        `总量: ${task.totalBytes ?? task.fileSize ?? 0}`,
        `速度: ${task.speedBytesPerSecond ?? 0}`,
        `错误: ${task.error ?? ''}`,
        '日志:',
        ...logLines,
        '',
      ].join('\n'),
    )
  }

  const blob = new Blob([sections.join('\n') || '暂无任务日志'], {
    type: 'text/plain;charset=utf-8',
  })
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = 'local-upload-agent-tasks.txt'
  anchor.click()
  URL.revokeObjectURL(url)
}

async function checkForUpdates() {
  const payload = await requestJson(`${UPDATE_URL}/check`, {
    method: 'POST',
  })
  renderUpdateState(payload.data ?? null)
}

async function installUpdate() {
  const payload = await requestJson(`${UPDATE_URL}/install`, {
    method: 'POST',
  })
  renderUpdateState(payload.data ?? null)
}

refreshButton?.addEventListener('click', () => {
  void refreshHealth()
  void refreshTasks()
  void refreshUpdateState()
})

pickFileButton?.addEventListener('click', () => {
  void pickFile()
})

exportLogsButton?.addEventListener('click', () => {
  void exportLogs()
})

checkUpdateButton?.addEventListener('click', () => {
  void checkForUpdates()
})

installUpdateButton?.addEventListener('click', () => {
  void installUpdate()
})

void refreshHealth()
void refreshTasks()
void refreshUpdateState()

window.setInterval(() => {
  void refreshHealth()
  void refreshTasks()
  void refreshUpdateState()
}, 2000)
