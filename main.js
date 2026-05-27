// ============================================================
// Sentinel - 메인 프로세스 (main.js)
// Electron 앱의 핵심 파일. 창 생성, 시스템 트레이, IPC 통신 담당
// ============================================================

const { app, BrowserWindow, ipcMain, screen, Tray, Menu, nativeImage, safeStorage } = require('electron');

// 창 목록 저장 객체 { 1: BrowserWindow, 2: BrowserWindow }
let windows = {};

// electron-store 인스턴스 (설정 저장용)
let store = null;

// 시스템 트레이 아이콘
let tray = null;

// 레이아웃 전환 중 앱 종료 방지 플래그
let isRelaunching = false;

// ── electron-store 지연 로딩 ──
// electron-store v8+ 은 ESM 전용이라 동적 import 필요
async function getStore() {
  if (!store) {
    const { default: Store } = await import('electron-store');
    store = new Store();
  }
  return store;
}

// ── 레이아웃 모드 읽기/쓰기 ──
async function getLayoutMode() {
  const s = await getStore();
  return s.get('layout-mode', 'single');
}

async function setLayoutMode(mode) {
  const s = await getStore();
  s.set('layout-mode', mode);
}

// ── 모니터 창 생성 ──
function createWindow(monitorId, display) {
  const { bounds } = display;

  const win = new BrowserWindow({
    x: bounds.x,
    y: bounds.y,
    width: bounds.width,
    height: bounds.height,
    frame: false,
    fullscreen: true,
    webPreferences: {
      nodeIntegration: true,
      contextIsolation: false,
      webviewTag: true,
      webSecurity: false,
      allowRunningInsecureContent: true,
      // 모니터별 독립 세션 - 쿠키/로그인 정보가 모니터마다 따로 유지됨
      partition: 'persist:monitor' + monitorId,
    },
    backgroundColor: '#080c10',
    title: 'Sentinel - MON-' + monitorId,
  });

  win.loadFile('index.html', { query: { monitorId: String(monitorId) } });
  windows[monitorId] = win;
  win.on('closed', () => { delete windows[monitorId]; });
  return win;
}

// ── 창 전체 재구성 ──
async function launchWindows() {
  isRelaunching = true;

  const oldWins = Object.values(windows);
  windows = {};
  oldWins.forEach(w => { try { w.destroy(); } catch(e){} });

  const displays = screen.getAllDisplays();
  const mode = await getLayoutMode();

  if (mode === 'single') {
    createWindow(1, screen.getPrimaryDisplay());
  } else {
    if (displays.length >= 2) {
      displays.slice(0, 2).forEach((display, i) => createWindow(i + 1, display));
    } else {
      createWindow(1, screen.getPrimaryDisplay());
    }
  }

  updateTray(mode, displays.length);
  isRelaunching = false;
}

// ── 트레이 메뉴 업데이트 ──
function updateTray(currentMode, displayCount) {
  if (!tray) return;
  const menu = Menu.buildFromTemplate([
    { label: 'Sentinel', enabled: false },
    { type: 'separator' },
    {
      label: '싱글 모니터',
      type: 'radio',
      checked: currentMode === 'single',
      click: async () => { await setLayoutMode('single'); await launchWindows(); }
    },
    {
      label: '듀얼 모니터' + (displayCount < 2 ? ' (모니터 1개 감지됨)' : ''),
      type: 'radio',
      checked: currentMode === 'dual',
      click: async () => { await setLayoutMode('dual'); await launchWindows(); }
    },
    { type: 'separator' },
    { label: '종료', click: () => { isRelaunching = false; app.quit(); } },
  ]);
  tray.setContextMenu(menu);
}

// ── 앱 시작 ──
app.whenReady().then(async () => {
  const icon = nativeImage.createEmpty();
  tray = new Tray(icon);
  tray.setToolTip('Sentinel');
  await launchWindows();
  screen.on('display-added', async () => await launchWindows());
  screen.on('display-removed', async () => await launchWindows());
});

// ── 앱 종료 처리 ──
app.on('window-all-closed', () => {
  if (isRelaunching) return;
  if (process.platform !== 'darwin') app.quit();
});

// ── IPC 핸들러 ──

// 모니터별 설정 불러오기
ipcMain.handle('load-settings', async (event, monitorId) => {
  const s = await getStore();
  return s.get('monitor-' + monitorId, null);
});

// 모니터별 설정 저장
ipcMain.on('save-settings', async (event, data) => {
  const s = await getStore();
  s.set('monitor-' + data.monitorId, data.settings);
});

// 전체화면 토글 + 상태 변경을 renderer에 전파
ipcMain.on('toggle-fullscreen', (event) => {
  const win = BrowserWindow.fromWebContents(event.sender);
  if (!win) return;
  const next = !win.isFullScreen();
  win.setFullScreen(next);
  // 전체화면 상태 변경을 renderer에 알려서 UI 업데이트
  win.webContents.send('fullscreen-changed', next);
});

// 창 최소화
ipcMain.on('minimize-window', (event) => {
  const win = BrowserWindow.fromWebContents(event.sender);
  if (win && !win.isMinimized()) {
    win.minimize();
  }
});

// 현재 전체화면 상태 반환
ipcMain.handle('get-fullscreen', (event) => {
  const win = BrowserWindow.fromWebContents(event.sender);
  return win ? win.isFullScreen() : false;
});

// 앱 종료
ipcMain.on('quit-app', () => { isRelaunching = false; app.quit(); });

// 레이아웃 모드 조회
ipcMain.handle('get-layout-mode', async () => {
  const displays = screen.getAllDisplays();
  return { mode: await getLayoutMode(), displayCount: displays.length };
});

// 레이아웃 모드 변경
ipcMain.on('set-layout-mode', async (event, mode) => {
  await setLayoutMode(mode);
  await launchWindows();
});

// ── 자격증명 암호화 저장 ──
// safeStorage를 사용해 OS 키체인 수준으로 암호화
ipcMain.handle('save-credentials', async (event, { key, username, password }) => {
  try {
    const s = await getStore();
    if (safeStorage.isEncryptionAvailable()) {
      // 암호화 가능한 환경 (일반적인 경우)
      const encUser = safeStorage.encryptString(username).toString('base64');
      const encPass = safeStorage.encryptString(password).toString('base64');
      s.set('cred-' + key, { encUser, encPass, encrypted: true });
    } else {
      // 암호화 불가 환경 fallback (base64만 적용)
      s.set('cred-' + key, {
        encUser: Buffer.from(username).toString('base64'),
        encPass: Buffer.from(password).toString('base64'),
        encrypted: false
      });
    }
    return { ok: true };
  } catch(e) {
    return { ok: false, error: e.message };
  }
});

// ── 자격증명 불러오기 ──
ipcMain.handle('load-credentials', async (event, key) => {
  try {
    const s = await getStore();
    const cred = s.get('cred-' + key, null);
    if (!cred) return null;
    if (cred.encrypted && safeStorage.isEncryptionAvailable()) {
      return {
        username: safeStorage.decryptString(Buffer.from(cred.encUser, 'base64')),
        password: safeStorage.decryptString(Buffer.from(cred.encPass, 'base64')),
      };
    } else {
      return {
        username: Buffer.from(cred.encUser, 'base64').toString(),
        password: Buffer.from(cred.encPass, 'base64').toString(),
      };
    }
  } catch(e) { return null; }
});

// ── 자격증명 존재 여부 확인 ──
ipcMain.handle('has-credentials', async (event, key) => {
  const s = await getStore();
  return s.has('cred-' + key);
});

// ── 자격증명 삭제 ──
ipcMain.handle('delete-credentials', async (event, key) => {
  const s = await getStore();
  s.delete('cred-' + key);
  return { ok: true };
});
