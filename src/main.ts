// Alyrion Launcher — frontend state projection.
// The UI is a pure function of the Rust `UiState` pushed over events
// (`state-changed`) plus an initial pull (`state_snapshot`). A slow poll
// loop acts as a safety net in case an event is ever missed.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { exit } from "@tauri-apps/plugin-process";

interface StageProgress {
  stage: string;
  fraction: number;
  detail: string;
}
interface InstalledInfo {
  version_number: string;
  version_id: string;
  mods: number;
}
interface SessionInfo {
  username: string;
  uuid: string;
  provider?: string;
}
interface JavaInfoUi {
  major: number;
  path: string;
}
interface UiState {
  phase:
    | "boot"
    | "checking"
    | "downloading"
    | "installing"
    | "ready"
    | "launching"
    | "running"
    | "error";
  progress: StageProgress | null;
  installed_version: InstalledInfo | null;
  latest_version: string | null;
  session: SessionInfo | null;
  java: JavaInfoUi | null;
  game_running: boolean;
  error: string | null;
}
interface NewsInfo {
  version_number: string;
  version_type: string;
  date_published: string;
  changelog: string;
}
interface SavedAcct {
  username: string;
  provider: string;
  uuid: string;
}

const $ = <T extends HTMLElement = HTMLElement>(id: string): T =>
  document.getElementById(id) as T;

const win = getCurrentWindow();

const PROV_LABEL: Record<string, string> = {
  offline: "Offline",
  littleskin: "LittleSkin",
  elyby: "Ely.by",
};

const STAGE_LABEL: Record<string, string> = {
  checking: "Checking for updates",
  fetching: "Downloading pack",
  downloading: "Downloading files",
  extracting: "Extracting pack",
  verifying: "Verifying files",
  finalizing: "Finalizing install",
  done: "Ready",
};

let lastState: UiState | null = null;
let toastTimer: ReturnType<typeof setTimeout> | undefined;

// ---------- Rendering ----------

function render(s: UiState) {
  lastState = s;

  // Version chips
  const chipVersion = $("chip-version");
  const ver = s.installed_version ?? null;
  chipVersion.textContent =
    ver?.version_number ?? (s.latest_version ? `v${s.latest_version}` : "—");

  // Status line
  const status = $("instance-status");
  const progressWrap = $("progress-wrap");
  const progressFill = $("progress-fill");
  const progressLabel = $("progress-label");
  const progressDetail = $("progress-detail");
  const errorBox = $("error-box");
  const btnPlay = $<HTMLButtonElement>("btn-play");
  const btnUpdate = $("btn-update");

  errorBox.hidden = true;
  progressWrap.hidden = true;

  if (s.phase === "error") {
    status.textContent = "Something went wrong";
    status.className = "instance-status err";
    $("error-text").textContent =
      s.error ?? "An unknown error occurred — try updating again.";
    errorBox.hidden = false;
    btnPlay.disabled = true;
    btnPlay.textContent = "Unavailable";
    btnPlay.classList.remove("btn-stop");
    btnUpdate.hidden = false;
  } else if (s.phase === "ready" || s.phase === "running") {
    if (s.game_running || s.phase === "running") {
      status.textContent = "Game is running";
      status.className = "instance-status ready";
      btnPlay.disabled = false;
      btnPlay.textContent = "STOP GAME";
      btnPlay.classList.add("btn-stop");
      btnUpdate.hidden = true;
    } else {
      status.textContent = "Ready to play";
      status.className = "instance-status ready";
      btnPlay.disabled = false;
      btnPlay.textContent = "PLAY";
      btnPlay.classList.remove("btn-stop");
      btnUpdate.hidden = true;
    }
  } else if (s.phase === "launching") {
    status.textContent = "Preparing launch…";
    status.className = "instance-status";
    btnPlay.disabled = true;
    btnPlay.textContent = "Launching…";
    btnPlay.classList.remove("btn-stop");
    btnUpdate.hidden = true;
  } else {
    // boot / checking / downloading / installing
    status.textContent = s.phase === "boot" ? "Starting up…" : "Updating…";
    status.className = "instance-status";
    const p = s.progress;
    if (p) {
      progressWrap.hidden = false;
      progressFill.style.width = `${Math.max(0, Math.min(1, p.fraction)) * 100}%`;
      progressLabel.textContent = STAGE_LABEL[p.stage] ?? p.stage ?? "Working…";
      progressDetail.textContent = p.detail ?? "";
    }
    btnPlay.disabled = true;
    btnPlay.textContent = "Updating…";
    btnUpdate.hidden = true;
  }

  // Meta
  $("meta-java").textContent = s.java
    ? `Java ${s.java.major} · ${shortPath(s.java.path)}`
    : "detecting…";
  $("meta-player").textContent = s.session
    ? `${s.session.username} · ${PROV_LABEL[s.session.provider ?? "offline"] ?? s.session.provider}`
    : "Not logged in";

  renderAccounts();
}

function shortPath(p: string): string {
  const parts = p.split(/[\\/]/);
  return parts.slice(-2).join("/");
}

// ---------- Accounts modal ----------

function setProvider(p: string) {
  const tabs = document.querySelectorAll<HTMLButtonElement>(".tab");
  tabs.forEach((t) => t.classList.toggle("active", t.dataset.provider === p));
  (["offline", "littleskin", "elyby"] as const).forEach((id) => {
    $("pane-" + id).hidden = id !== p;
  });
  // Move focus to the first input of the visible pane.
  const pane = $<HTMLElement>("pane-" + p);
  const firstInput = pane.querySelector<HTMLInputElement>("input");
  if (firstInput && !firstInput.value) firstInput.focus();
}

function openModal() {
  $("accounts-modal").hidden = false;
  renderAccounts();
  setProvider(currentProvider());
}

function currentProvider(): string {
  const active = document.querySelector<HTMLButtonElement>(".tab.active");
  return active?.dataset.provider ?? "offline";
}

function closeModal() {
  $("accounts-modal").hidden = true;
  // Re-render footer in case the session changed while the modal was open.
  if (lastState) render(lastState);
}

function renderAccounts() {
  const section = $("saved-accounts-section");
  const list = $("saved-accounts-list");
  const signOut = $<HTMLButtonElement>("btn-signout");
  const s = lastState;

  const accs = accountCache;
  section.hidden = accs.length === 0;
  signOut.hidden = accs.length === 0;
  signOut.disabled = false;

  if (accs.length === 0) {
    list.innerHTML = "";
    return;
  }
  const active =
    s?.session
      ? { username: s.session.username, provider: s.session.provider ?? "" }
      : null;
  list.innerHTML = accs
    .map((a) => {
      const isActive =
        !!active &&
        a.username === active.username &&
        a.provider === active.provider;
      const label = PROV_LABEL[a.provider] ?? a.provider;
      return `<div class="saved-item${isActive ? " active" : ""}">
               <span class="saved-name">${escapeHtml(a.username)}</span>
               <span class="prov">${escapeHtml(label)}${isActive ? " · active" : ""}</span>
             </div>`;
    })
    .join("");
}

let accountCache: SavedAcct[] = [];

async function refreshAccounts() {
  try {
    accountCache = await invoke<SavedAcct[]>("list_accounts");
  } catch (e) {
    accountCache = [];
    console.error("list_accounts failed", e);
  }
  renderAccounts();
}

async function signOutAll() {
  const btn = $<HTMLButtonElement>("btn-signout");
  btn.disabled = true;
  try {
    await invoke("logout");
    accountCache = [];
    renderAccounts();
    toast("Signed out");
  } catch (e) {
    toast(String(e));
  } finally {
    btn.disabled = false;
  }
}

// ---------- Login forms ----------

async function submitLogin(
  btn: HTMLButtonElement,
  submit: () => Promise<unknown>,
) {
  if (btn.disabled) return;
  const original = btn.textContent;
  btn.disabled = true;
  btn.classList.add("is-busy");
  btn.textContent = "Signing in…";
  try {
    await submit();
    closeModal();
  } catch (e) {
    toast(String(e));
  } finally {
    btn.disabled = false;
    btn.classList.remove("is-busy");
    btn.textContent = original;
  }
}

function bindLoginButton(
  btnId: string,
  submit: () => Promise<unknown>,
  enterIds: string[],
) {
  const btn = $<HTMLButtonElement>(btnId);
  btn.addEventListener("click", () => submitLogin(btn, submit));
  enterIds.forEach((id) => {
    $<HTMLInputElement>(id).addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        submitLogin(btn, submit);
      }
    });
  });
}

// ---------- Toast ----------

function toast(msg: string) {
  $("toast-text").textContent = msg;
  const el = $("toast");
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    el.hidden = true;
  }, 6000);
}

// ---------- Changelog ----------

function renderMarkdown(md: string): string {
  const esc = escapeHtml(md);
  const lines = esc.split(/\r?\n/);
  const out: string[] = [];
  let inList = false;
  const closeList = () => {
    if (inList) {
      out.push("</ul>");
      inList = false;
    }
  };
  for (const raw of lines) {
    const line = raw.trimEnd();
    const heading = line.match(/^(#{1,4})\s+(.*)$/);
    if (heading) {
      closeList();
      out.push(`<h3>${inline(heading[2]!)}</h3>`);
      continue;
    }
    const item = line.match(/^\s*[-*]\s+(.*)$/);
    if (item) {
      if (!inList) {
        out.push("<ul>");
        inList = true;
      }
      out.push(`<li>${inline(item[1]!)}</li>`);
      continue;
    }
    if (line.trim() === "") {
      closeList();
      continue;
    }
    closeList();
    out.push(`<p>${inline(line)}</p>`);
  }
  closeList();
  return out.join("");
}

function inline(text: string): string {
  // Bold, then https-only links (safe: input is already HTML-escaped).
  const bolded = text.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
  return bolded.replace(
    /\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)/g,
    '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>',
  );
}

async function loadChangelog() {
  const panel = $("changelog");
  const meta = $("news-meta");
  try {
    const news = await invoke<NewsInfo>("latest_changelog");
    meta.hidden = false;
    meta.innerHTML = `<span class="news-version">v${escapeHtml(news.version_number)}</span>` +
      `<span class="news-date">${escapeHtml(news.version_type)}</span>` +
      `<span class="news-date">${formatDate(news.date_published)}</span>`;
    const body = news.changelog.trim();
    panel.innerHTML = body
      ? renderMarkdown(body)
      : '<p class="muted">No release notes for this version.</p>';
  } catch (e) {
    console.error("latest_changelog failed", e);
    panel.innerHTML =
      '<p class="muted">Could not load release notes — check your connection or open Modrinth ↗.</p>';
  }
}

function formatDate(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return d.toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

// ---------- Boot ----------

function bindWindowControls() {
  const header = document.querySelector<HTMLElement>(".titlebar");
  header?.addEventListener("mousedown", (e) => {
    if ((e.target as HTMLElement).closest(".titlebar-right")) return;
    if (e.buttons === 1) {
      win.startDragging();
    }
  });

  $("win-min").addEventListener("click", () => win.minimize());
  $("win-max").addEventListener("click", async () => {
    if (await win.isMaximized()) await win.unmaximize();
    else await win.maximize();
  });
  $("win-close").addEventListener("click", () => exit(0));
}

function bindMainActions() {
  $("btn-play").addEventListener("click", async () => {
    const isRunning = lastState?.game_running || lastState?.phase === "running";
    if (isRunning) {
      try {
        ($("btn-play") as HTMLButtonElement).disabled = true;
        ($("btn-play") as HTMLButtonElement).textContent = "Stopping…";
        await invoke("kill_game");
      } catch (e) {
        toast("Failed to stop game: " + String(e));
        if (lastState) render(lastState);
      }
    } else {
      try {
        await invoke("play");
      } catch (e) {
        toast(String(e));
      }
    }
  });
  $("btn-update").addEventListener("click", async () => {
    try {
      await invoke("start_update");
    } catch (e) {
      toast(String(e));
    }
  });
  $("open-modrinth").addEventListener("click", (e) => {
    e.preventDefault();
    window.open("https://modrinth.com/modpack/alyrion", "_blank");
  });
  $("toast-close").addEventListener("click", () => {
    clearTimeout(toastTimer);
    $("toast").hidden = true;
  });
}

function bindModal() {
  const modal = $("accounts-modal");
  $("btn-account").addEventListener("click", () => {
    openModal();
  });
  $("modal-close").addEventListener("click", closeModal);
  modal.addEventListener("click", (e) => {
    if (e.target === modal) closeModal();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !modal.hidden) closeModal();
  });

  // Provider tabs.
  document.querySelectorAll<HTMLButtonElement>(".tab").forEach((t) =>
    t.addEventListener("click", () =>
      setProvider(t.dataset.provider ?? "offline"),
    ),
  );

  // Sign out.
  $("btn-signout").addEventListener("click", signOutAll);
}

function bindLoginForms() {
  bindLoginButton("btn-offline", async () => {
    const u = ($("offline-username") as HTMLInputElement).value.trim();
    if (!/^[A-Za-z0-9_]{3,16}$/.test(u)) {
      throw new Error("Name must be 3–16 letters/digits/underscores");
    }
    await invoke("login_offline", { username: u });
    accountCache = [];
    await refreshAccounts();
  }, ["offline-username"]);

  bindLoginButton("btn-littleskin", async () => {
    const u = ($("ls-username") as HTMLInputElement).value.trim();
    const p = ($("ls-password") as HTMLInputElement).value;
    if (!u || !p) throw new Error("Enter username and password");
    await invoke("login_littleskin", { username: u, password: p });
    accountCache = [];
    await refreshAccounts();
  }, ["ls-username", "ls-password"]);

  bindLoginButton("btn-elyby", async () => {
    const u = ($("eb-username") as HTMLInputElement).value.trim();
    const p = ($("eb-password") as HTMLInputElement).value;
    if (!u || !p) throw new Error("Enter username and password");
    await invoke("login_elyby", { username: u, password: p });
    accountCache = [];
    await refreshAccounts();
  }, ["eb-username", "eb-password"]);
}

interface Settings {
  littleskin_server: string;
  allocated_memory_mb: number;
  jvm_args: string;
}

function formatRamDisplay(mb: number): string {
  const gb = (mb / 1024).toFixed(1);
  return `${mb} MB (${gb} GB)`;
}

function openSettingsModal() {
  $("settings-modal").hidden = false;
  void loadSettingsIntoForm();
}

function closeSettingsModal() {
  $("settings-modal").hidden = true;
}

async function loadSettingsIntoForm() {
  try {
    const s = await invoke<Settings>("get_settings");
    const slider = $("ram-slider") as HTMLInputElement;
    slider.value = String(s.allocated_memory_mb || 4096);
    $("ram-val").textContent = formatRamDisplay(Number(slider.value));
    ($("jvm-args") as HTMLTextAreaElement).value = s.jvm_args || "";
    ($("settings-littleskin") as HTMLInputElement).value = s.littleskin_server || "https://littleskin.cn/api/yggdrasil";
  } catch (e) {
    toast("Failed to load settings: " + String(e));
  }
}

async function saveSettingsFromForm() {
  const btn = $("btn-save-settings") as HTMLButtonElement;
  btn.disabled = true;
  try {
    const slider = $("ram-slider") as HTMLInputElement;
    const ram = Math.max(1024, Math.min(65536, parseInt(slider.value, 10) || 4096));
    const jvm = ($("jvm-args") as HTMLTextAreaElement).value.trim();
    const ls = ($("settings-littleskin") as HTMLInputElement).value.trim() || "https://littleskin.cn/api/yggdrasil";

    const settings: Settings = {
      littleskin_server: ls,
      allocated_memory_mb: ram,
      jvm_args: jvm,
    };

    await invoke("save_settings", { settings });
    closeSettingsModal();
    toast("Settings saved");
  } catch (e) {
    toast("Failed to save settings: " + String(e));
  } finally {
    btn.disabled = false;
  }
}

function bindSettingsModal() {
  const modal = $("settings-modal");
  $("btn-settings").addEventListener("click", openSettingsModal);
  $("settings-close").addEventListener("click", closeSettingsModal);
  modal.addEventListener("click", (e) => {
    if (e.target === modal) closeSettingsModal();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !modal.hidden) closeSettingsModal();
  });

  const slider = $("ram-slider") as HTMLInputElement;
  slider.addEventListener("input", () => {
    $("ram-val").textContent = formatRamDisplay(Number(slider.value));
  });

  $("btn-save-settings").addEventListener("click", () => {
    void saveSettingsFromForm();
  });
}

async function pull() {
  try {
    render(await invoke<UiState>("state_snapshot"));
  } catch (e) {
    console.error("state_snapshot failed", e);
  }
}

async function startStateStream() {
  try {
    await listen<UiState>("state-changed", (e) => render(e.payload));
  } catch (e) {
    console.error("state-changed listener failed, falling back to polling", e);
  }
  await pull();
  // Safety net: re-pull periodically so a missed event can never leave the
  // UI stale (render is idempotent, so this is always safe).
  setInterval(pull, 2000);
}

async function boot() {
  bindWindowControls();
  bindMainActions();
  bindModal();
  bindSettingsModal();
  bindLoginForms();
  await startStateStream();
  void refreshAccounts();
  void loadChangelog();
}

boot().catch((e) => {
  console.error("boot failed", e);
  toast("Frontend failed to start: " + String(e));
});

// ---------- Helpers ----------

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
}

export {};
