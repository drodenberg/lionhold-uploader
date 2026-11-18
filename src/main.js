const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open } = window.__TAURI__.dialog;

const $ = (id) => document.getElementById(id);

function ago(seconds) {
  if (!seconds) return "";
  const mins = Math.round((Date.now() / 1000 - seconds) / 60);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins} min ago`;
  if (mins < 60 * 48) return `${Math.round(mins / 60)} hours ago`;
  return `${Math.round(mins / 1440)} days ago`;
}

function render(s) {
  $("version").textContent = `Version ${s.version}.`;

  $("dot-wow").className = `dot ${s.wowDir ? "ok" : "bad"}`;
  $("wow-text").textContent = s.wowDir ?? "Couldn't find WoW. Choose the main \"World of Warcraft\" folder.";
  $("addon-text").textContent = s.wowDir ? `Lionhold Export addon: ${s.addon}` : "";
  $("choose").textContent = s.wowDir ? "Change folder…" : "Choose folder…";

  const linking = !!s.linkingCode;
  $("dot-account").className = `dot ${s.connected ? "ok" : linking ? "wait" : "bad"}`;
  $("account-text").textContent = s.connected
    ? `Connected${s.battletag ? ` as ${s.battletag}` : ""}.`
    : linking
      ? "Finish in your browser: click \"Connect this computer\". The page shows this code:"
      : "Not connected. Connect opens Lionhold Armory; sign in with Battle.net and approve.";
  $("code-text").hidden = !linking;
  $("code-text").textContent = s.linkingCode ?? "";
  $("connect").hidden = s.connected;
  $("connect").disabled = linking;
  $("connect").textContent = linking ? "Waiting for approval…" : "Connect";
  $("disconnect").hidden = !s.connected;
  $("share-row").hidden = !s.connected;
  $("share").checked = !!s.share;

  $("dot-upload").className = `dot ${s.lastUploadAt ? "ok" : "wait"}`;
  $("upload-text").textContent = s.lastUploadAt
    ? `Last upload ${ago(s.lastUploadAt)}.`
    : "Nothing uploaded yet. Log out of a character once (or type /reload).";
  $("upload").disabled = !s.connected || !s.wowDir;
  const message = s.lastMessage ?? "";
  $("message").textContent = message;
  $("message").className = `small ${/couldn't|disconnected|expired|error/i.test(message) ? "error" : "muted"}`;
}

$("choose").addEventListener("click", async () => {
  const picked = await open({ directory: true, title: "Choose your World of Warcraft folder" });
  if (!picked) return;
  try {
    render(await invoke("set_wow_dir", { path: picked }));
  } catch (e) {
    $("message").textContent = String(e);
    $("message").className = "small error";
  }
});
$("connect").addEventListener("click", async () => {
  try {
    await invoke("connect");
  } catch (e) {
    $("message").textContent = String(e);
    $("message").className = "small error";
  }
});
$("share").addEventListener("change", async (e) => {
  try {
    render(await invoke("set_share", { share: e.target.checked }));
  } catch (err) {
    e.target.checked = !e.target.checked;
    $("message").textContent = String(err);
    $("message").className = "small error";
  }
});
$("disconnect").addEventListener("click", async () => render(await invoke("disconnect")));
$("upload").addEventListener("click", async () => {
  $("upload").disabled = true;
  $("upload").textContent = "Uploading…";
  render(await invoke("upload_now"));
  $("upload").textContent = "Upload now";
});

listen("status", (e) => render(e.payload));
invoke("get_status").then(render);
// Keep "x min ago" fresh.
setInterval(() => invoke("get_status").then(render), 30000);
