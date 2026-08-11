// Alyrion Launcher — frontend state projection.
// The UI is a pure function of the Rust `UiState` pushed over events
// (`state-changed`) plus an initial pull (`state_snapshot`).

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

const $ = <T extends HTMLElement = HTMLElement>(id: string): T =>
  document.getElementById(id) as T;

const win = getCurrentWindow();

const STAGE_LABEL: Record<string, string> = {
  checking: "Checking for updates",
  fetching: "Downloading pack",
  downloading: "Downloading files",
  extracting: "Extracting pack",
  verifying: "Verifying files",
  finalizing: "Finalizing install",
  done: "Ready",
};

function render(s: UiState) {
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
  const btnPlay = $<HTMLButtonElement>("btn-play");
  const btnUpdate = $("btn-update");

  if (s.phase === "error") {
    status.textContent = "Something went wrong";
    status.className = "instance-status err";
    progressWrap.hidden = true;
    btnPlay.disabled = true;
    btnPlay.textContent = "Update failed";
    btnUpdate.hidden = false;
  } else if (s.phase === "ready") {
    status.textContent = "Ready to play";
    status.className = "instance-status ready";
    progressWrap.hidden = true;
    btnPlay.disabled = false;
    btnPlay.textContent = s.game_running ? "Playing…" : "PLAY";
    btnUpdate.hidden = true;
  } else if (s.phase === "running") {
    status.textContent = "Game is running";
    status.className = "instance-status ready";
    progressWrap.hidden = true;
    btnPlay.disabled = true;
    btnPlay.textContent = "In game…";
    btnUpdate.hidden = true;
  } else if (s.phase === "launching") {
    status.textContent = "Preparing launch…";
    status.className = "instance-status";
    progressWrap.hidden = true;
    btnPlay.disabled = true;
    btnPlay.textContent = "Launching…";
    btnUpdate.hidden = true;
  } else {
    // boot / checking / downloading / installing
    status.textContent = s.phase === "boot" ? "Starting up…" : "Updating…";
    status.className = "instance-status";
    const p = s.progress;
    if (p) {
      progressWrap.hidden = false;
      progressFill.style.width = `${Math.round(p.fraction * 100)}%`;
      progressLabel.textContent =
        STAGE_LABEL[p.stage] ?? p.stage ?? "Working…";
      progressDetail.textContent = p.detail ?? "";
    } else {
      progressWrap.hidden = true;
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
    ? `${s.session.username} · ${s.session.provider ?? "offline"}`
    : "Not logged in";
}

function shortPath(p: string): string {
  const parts = p.split(/[\\/]/);
  return parts.slice(-2).join("/");
}

async function pull() {
  try {
    const s = await invoke<UiState>("state_snapshot");
    render(s);
  } catch (e) {
    console.error("state_snapshot failed", e);
  }
}

async function boot() {
  // Window controls
  $("win-min").addEventListener("click", () => win.minimize());
  $("win-max").addEventListener("click", async () => {
    if (await win.isMaximized()) await win.unmaximize();
    else await win.maximize();
  });
  $("win-close").addEventListener("click", () => exit(0));

  // Play
  $("btn-play").addEventListener("click", async () => {
    try {
      await invoke("play");
    } catch (e) {
      toast(String(e));
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

  // Toast close
  $("toast-close").addEventListener("click", () => ($("toast").hidden = true));

  // Accounts modal
  const modal = $("accounts-modal");
  $("btn-account").addEventListener("click", () => {
    modal.hidden = false;
    refreshAccounts();
  });
  $("modal-close").addEventListener("click", () => (modal.hidden = true));
  modal.addEventListener("click", (e) => {
    if (e.target === modal) modal.hidden = true;
  });

  // Provider tabs
  const tabs = document.querySelectorAll<HTMLButtonElement>(".tab");
  const setProvider = (p: string) => {
    tabs.forEach((t) => t.classList.toggle("active", t.dataset.provider === p));
    ["offline", "littleskin", "elyby"].forEach((id) => {
      $("pane-" + id).hidden = id !== p;
    });
  };
  tabs.forEach((t) => t.addEventListener("click", () => setProvider(t.dataset.provider ?? "offline")));
  setProvider("offline");

  // Offline login
  $("btn-offline").addEventListener("click", async () => {
    const u = ($("offline-username") as HTMLInputElement).value.trim();
    if (!/^[A-Za-z0-9_]{3,16}$/.test(u)) {
      toast("Name must be 3–16 letters/digits/underscores");
      return;
    }
    try {
      await invoke("login_offline", { username: u });
      modal.hidden = true;
    } catch (e) {
      toast(String(e));
    }
  });

  // LittleSkin login
  $("btn-littleskin").addEventListener("click", async () => {
    const u = ($("ls-username") as HTMLInputElement).value.trim();
    const p = ($("ls-password") as HTMLInputElement).value;
    if (!u || !p) {
      toast("Enter username and password");
      return;
    }
    try {
      await invoke("login_littleskin", { username: u, password: p });
      modal.hidden = true;
    } catch (e) {
      toast(String(e));
    }
  });

  // Ely.by login (direct credentials, Yggdrasil)
  $("btn-elyby").addEventListener("click", async () => {
    const u = ($("eb-username") as HTMLInputElement).value.trim();
    const p = ($("eb-password") as HTMLInputElement).value;
    if (!u || !p) {
      toast("Enter username and password");
      return;
    }
    try {
      await invoke("login_elyby", { username: u, password: p });
      modal.hidden = true;
    } catch (e) {
      toast(String(e));
    }
  });

  // Listen for state changes pushed from Rust
  await listen<UiState>("state-changed", (e) => render(e.payload));
  await pull();
}

interface SavedAcct {
  username: string;
  provider: string;
  uuid: string;
}

async function refreshAccounts() {
  try {
    const accs = await invoke<SavedAcct[]>("list_accounts");
    const list = $("saved-accounts-list");
    if (accs.length === 0) {
      list.innerHTML = '<div class="empty">No saved accounts yet.</div>';
      return;
    }
    const provLabel: Record<string, string> = { offline: "Offline", littleskin: "LittleSkin", elyby: "Ely.by" };
    list.innerHTML = accs
      .map(
        (a) =>
          `<div class="saved-item" data-uuid="${a.uuid}">
             <span>${escapeHtml(a.username)} <span class="prov">· ${provLabel[a.provider] ?? a.provider}</span></span>
             <button data-kind="remove" title="Remove">×</button>
           </div>`
      )
      .join("");
    list.querySelectorAll('button[data-kind="remove"]').forEach((b) =>
      b.addEventListener("click", () => {
        logoutAll();
      })
    );
  } catch (e) {
    console.error("list_accounts failed", e);
  }
}

function logoutAll() {
  invoke("logout", {})
    .then(() => refreshAccounts())
    .catch((e) => toast(String(e)));
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
}

function toast(msg: string) {
  $("toast-text").textContent = msg;
  $("toast").hidden = false;
}

boot().catch((e) => {
  console.error("boot failed", e);
  toast("Frontend failed to start: " + String(e));
});

declare global {
  // Vite CSS import
}
export {};