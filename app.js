import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

const $ = (s, p = document) => p.querySelector(s);
const $$ = (s, p = document) => [...p.querySelectorAll(s)];

let files = [];
let selectedIndex = -1;
let dragIndex = null;
let sortAsc = true;
let thumbDir = null;  // set when a .kag project is loaded

const thumbCache = new Map();
const previewCache = new Map();
let previewLoading = false;

/* Tiny transparent pixel to prevent broken icon */
const PIXEL_SRC =
  "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";

/* ─── Thumbnail loader (paused during scroll) ─── */
let thumbLoading = false;
const thumbPending = [];
let scrollPauseTimer = null;
let scrollPaused = false;

function loadNextThumb() {
  if (scrollPaused || thumbLoading || thumbPending.length === 0) return;
  const job = thumbPending.shift();
  thumbLoading = true;
  const cached = thumbCache.get(job.path);
  if (cached) {
    doSetThumb(job.imgEl, cached).then(() => { thumbLoading = false; loadNextThumb(); });
    return;
  }
  const useProject = thumbDir && files.indexOf(job.path) >= 0;
  const promise = useProject
    ? invoke("read_project_thumb", { thumbDir, index: files.indexOf(job.path) })
    : invoke("get_thumbnail", { path: job.path, maxSize: 800 });
  promise
    .then((b64) => {
      const url = `data:image/jpeg;base64,${b64}`;
      thumbCache.set(job.path, url);
      return doSetThumb(job.imgEl, url);
    })
    .catch(() => { job.imgEl.classList.add("loaded"); })
    .finally(() => { thumbLoading = false; loadNextThumb(); });
}

async function doSetThumb(el, url) {
  el.src = url;
  try { await el.decode(); } catch (_) {}
  el.classList.add("loaded");
}

function queueThumb(path, imgEl) {
  imgEl.src = PIXEL_SRC;
  thumbPending.push({ path, imgEl });
  if (!scrollPaused) loadNextThumb();
}

/* ─── Targeted DOM operations (no full rebuilds) ─── */

function createItemEl(i) {
  const f = files[i];
  const parts = f.replace(/\\/g, "/").split("/");
  const name = parts[parts.length - 1];
  const div = document.createElement("div");
  div.className = `img-item${i === selectedIndex ? " selected" : ""}`;
  div.draggable = true;
  div.dataset.index = i;
  div.innerHTML = `
    <span class="img-drag-handle">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><circle cx="9" cy="5" r="1.5"/><circle cx="15" cy="5" r="1.5"/><circle cx="9" cy="12" r="1.5"/><circle cx="15" cy="12" r="1.5"/><circle cx="9" cy="19" r="1.5"/><circle cx="15" cy="19" r="1.5"/></svg>
    </span>
    <img class="img-thumb" src="${PIXEL_SRC}" alt="" />
    <span class="img-name">${name}</span>
    <span class="img-index">#${i + 1}</span>`;
  div.addEventListener("click", () => selectItem(i));
  div.addEventListener("dragstart", onDragStart);
  div.addEventListener("dragend", onDragEnd);
  div.addEventListener("dragover", onDragOver);
  div.addEventListener("dragleave", onDragLeave);
  div.addEventListener("drop", onDrop);
  queueThumb(f, div.querySelector(".img-thumb"));
  return div;
}

function updateDataIndices(from) {
  const kids = elList.children;
  for (let j = from; j < kids.length; j++) kids[j].dataset.index = j;
}

function appendItemsToDOM(start, end) {
  const frag = document.createDocumentFragment();
  for (let i = start; i < end; i++) frag.appendChild(createItemEl(i));
  elList.appendChild(frag);
  updateMeta();
}

function removeItemFromDOM(i) {
  const el = elList.children[i];
  if (el) { el.remove(); updateDataIndices(i); }
  if (files.length === 0) showEmptyState();
  updateMeta();
}

function insertItemInDOM(i, el) {
  const next = elList.children[i];
  if (next) elList.insertBefore(el, next);
  else elList.appendChild(el);
  updateDataIndices(i);
  updateMeta();
}

function swapItemsInDOM(a, b) {
  const kids = elList.children;
  if (a < 0 || a >= kids.length || b < 0 || b >= kids.length) return;
  if (a === b) return;
  const elA = kids[a];
  const elB = kids[b];
  if (b > a) {
    elA.parentNode.insertBefore(elB, elA);
    elA.parentNode.insertBefore(elA, elB.nextSibling);
  } else {
    elB.parentNode.insertBefore(elA, elB);
    elB.parentNode.insertBefore(elB, elA.nextSibling);
  }
  elA.dataset.index = b;
  elB.dataset.index = a;
}

function updateDOMSelection(oldIdx, newIdx) {
  const kids = elList.children;
  if (oldIdx >= 0 && oldIdx < kids.length) kids[oldIdx].classList.remove("selected");
  if (newIdx >= 0 && newIdx < kids.length) kids[newIdx].classList.add("selected");
}

function showEmptyState() {
  elList.innerHTML = `
    <div class="empty-state">
      <svg width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1" opacity="0.25"><rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="8.5" cy="8.5" r="1.5"/><path d="m21 15-5-5L5 21"/></svg>
      <p>拖拽图片到此处<br/>或点击上方按钮添加</p>
    </div>`;
  $$(".img-drag-handle").forEach(h => h.style.display = "none");
}

/* ─── Full rebuild (initial load, open project) ─── */
function rebuildList() {
  elList.innerHTML = "";
  if (files.length === 0) { showEmptyState(); updateMeta(); return; }
  const CHUNK = 30;
  let idx = 0;
  function appendChunk() {
    const end = Math.min(idx + CHUNK, files.length);
    appendItemsToDOM(idx, end);
    idx = end;
    if (idx < files.length) requestAnimationFrame(appendChunk);
    else {
      if (selectedIndex >= files.length) selectedIndex = files.length - 1;
      if (selectedIndex >= 0) previewImage(selectedIndex);
      $$(".img-drag-handle").forEach(h => {
        h.style.display = files.length > 1 ? "" : "none";
      });
    }
  }
  requestAnimationFrame(appendChunk);
}

/* ─── DOM ─── */
const elList = $("#image-list");
elList.addEventListener("scroll", () => {
  if (!scrollPaused) scrollPaused = true;
  clearTimeout(scrollPauseTimer);
  scrollPauseTimer = setTimeout(() => {
    scrollPaused = false;
    loadNextThumb();
  }, 200);
}, { passive: true });
const elBadge = $("#file-badge");
const elCount = $("#file-count-label");
const elPreview = $("#preview-image");
const elPreviewWrap = $("#preview-wrap");
const elPlaceholder = $("#preview-placeholder");
const elBtnExport = $("#btn-export");
const elProgressArea = $("#progress-area");
const elProgressFill = $("#progress-fill");
const elProgressText = $("#progress-text");

/* ─── Toast ─── */
function toast(msg, type = "success") {
  let el = $("#toast");
  if (!el) {
    el = document.createElement("div");
    el.id = "toast";
    document.body.appendChild(el);
  }
  el.textContent = msg;
  el.className = `show ${type}`;
  clearTimeout(el._t);
  el._t = setTimeout(() => el.classList.remove("show"), 2800);
}

/* ─── File Actions ─── */
async function addFiles() {
  const sel = await open({
    multiple: true,
    filters: [{ name: "图片", extensions: ["jpg","jpeg","png","webp","bmp","gif","tiff","tif"] }],
  });
  if (!sel) return;
  thumbDir = null;
  const start = files.length;
  for (const f of sel) {
    if (!files.includes(f)) files.push(f);
  }
  if (files.length > start) {
    appendItemsToDOM(start, files.length);
    if (selectedIndex < 0 && files.length > 0) { selectItem(0); }
    $$(".img-drag-handle").forEach(h => h.style.display = files.length > 1 ? "" : "none");
  }
}

async function addFolder() {
  try {
    const dir = await open({ directory: true });
    if (!dir) return;

    elList.innerHTML = `
      <div class="loading-overlay">
        <div class="loading-spinner"></div>
        <span>正在扫描文件夹…</span>
      </div>`;

    thumbDir = null;
    const discovered = await invoke("scan_directory", { path: dir });
    let added = 0;
    for (const f of discovered) {
      if (!files.includes(f)) { files.push(f); added++; }
    }
    if (added === 0) return;
    rebuildList();
  } catch (e) {
    console.error(e);
    toast("扫描文件夹失败: " + e, "error");
    if (files.length === 0) showEmptyState();
  }
}

function updateMeta() {
  elBadge.textContent = files.length;
  elCount.textContent = `${files.length} 张`;
  updateButtons();
}

/* ─── Drag & Drop ─── */
function onDragStart(e) {
  const item = e.currentTarget;
  dragIndex = Number(item.dataset.index);
  item.classList.add("dragging");
  e.dataTransfer.effectAllowed = "move";
  const ghost = item.cloneNode(true);
  ghost.style.position = "absolute"; ghost.style.top = "-1000px";
  ghost.style.width = "260px";
  document.body.appendChild(ghost);
  e.dataTransfer.setDragImage(ghost, 20, 28);
  setTimeout(() => document.body.removeChild(ghost), 0);
}
function onDragEnd(e) {
  e.currentTarget.classList.remove("dragging");
  dragIndex = null;
  $$(".img-item.drag-over").forEach((el) => el.classList.remove("drag-over"));
}
function onDragOver(e) {
  e.preventDefault(); e.dataTransfer.dropEffect = "move";
  $$(".img-item.drag-over").forEach((el) => el.classList.remove("drag-over"));
  e.currentTarget.classList.add("drag-over");
}
function onDragLeave(e) { e.currentTarget.classList.remove("drag-over"); }
function onDrop(e) {
  e.preventDefault();
  const target = e.currentTarget;
  target.classList.remove("drag-over");
  const targetIndex = Number(target.dataset.index);
  if (dragIndex !== null && dragIndex !== targetIndex) {
    const [moved] = files.splice(dragIndex, 1);
    const t = targetIndex > dragIndex ? targetIndex - 1 : targetIndex;
    files.splice(t, 0, moved);
    if (selectedIndex === dragIndex) selectedIndex = t;
    else if (selectedIndex > dragIndex && selectedIndex <= t) selectedIndex--;
    else if (selectedIndex < dragIndex && selectedIndex >= t) selectedIndex++;
    rebuildList();
  }
}

/* ─── Select / Preview ─── */
function selectItem(idx) {
  selectedIndex = idx;
  $$(".img-item").forEach((el, i) => el.classList.toggle("selected", i === idx));
  updateButtons();
  previewImage(idx);
}

function previewImage(idx) {
  const path = files[idx];
  if (!path) return;
  const fromPreview = previewCache.get(path);
  if (fromPreview) { showPreview(fromPreview); return; }
  const fromThumb = thumbCache.get(path);
  if (fromThumb) { showPreview(fromThumb); return; }
  if (previewLoading) return;
  previewLoading = true;
  invoke("get_thumbnail", { path, maxSize: 800 })
    .then((b64) => {
      const url = `data:image/jpeg;base64,${b64}`;
      thumbCache.set(path, url);
      showPreview(url);
    })
    .catch(() => {})
    .finally(() => { previewLoading = false; });
}

function showPreview(url) {
  elPreview.src = url;
  elPreviewWrap.style.display = "flex";
  elPlaceholder.style.display = "none";
  elPreview.style.animation = "none";
  requestAnimationFrame(() => { elPreview.style.animation = "previewIn 0.3s ease"; });
}

function updateButtons() {
  const has = selectedIndex >= 0 && selectedIndex < files.length;
  $("#btn-move-up").disabled = !has || selectedIndex === 0;
  $("#btn-move-down").disabled = !has || selectedIndex === files.length - 1;
  $("#btn-remove").disabled = !has;
}

/* ─── Move / Remove ─── */
function moveUp() {
  if (selectedIndex <= 0) return;
  [files[selectedIndex], files[selectedIndex - 1]] = [files[selectedIndex - 1], files[selectedIndex]];
  const newIdx = selectedIndex - 1;
  swapItemsInDOM(selectedIndex, newIdx);
  selectedIndex = newIdx;
  updateDOMSelection(newIdx + 1, newIdx);
  updateButtons();
}
function moveDown() {
  if (selectedIndex >= files.length - 1) return;
  [files[selectedIndex], files[selectedIndex + 1]] = [files[selectedIndex + 1], files[selectedIndex]];
  const newIdx = selectedIndex + 1;
  swapItemsInDOM(selectedIndex, newIdx);
  selectedIndex = newIdx;
  updateDOMSelection(newIdx - 1, newIdx);
  updateButtons();
}
function removeSelected() {
  if (selectedIndex < 0 || selectedIndex >= files.length) return;
  const idx = selectedIndex;
  files.splice(idx, 1);
  removeItemFromDOM(idx);
  if (selectedIndex >= files.length) selectedIndex = files.length - 1;
  if (selectedIndex >= 0) updateDOMSelection(-1, selectedIndex);
  updateButtons();
  if (files.length === 0) {
    elPreviewWrap.style.display = "none";
    elPlaceholder.style.display = "flex";
  }
}
function clearAll() {
  files = [];
  selectedIndex = -1;
  thumbDir = null;
  elPreviewWrap.style.display = "none";
  elPlaceholder.style.display = "flex";
  showEmptyState();
  updateMeta();
}
function sortFiles() {
  const selectedPath = selectedIndex >= 0 ? files[selectedIndex] : null;
  const dir = sortAsc ? 1 : -1;
  files.sort((a, b) => {
    const na = a.split(/[/\\]/).pop().toLowerCase();
    const nb = b.split(/[/\\]/).pop().toLowerCase();
    return na < nb ? -dir : na > nb ? dir : 0;
  });
  sortAsc = !sortAsc;
  selectedIndex = selectedPath ? files.indexOf(selectedPath) : -1;
  rebuildList();
}

/* ─── Choose Output Dir ─── */
async function chooseOutDir() {
  const dir = await open({ directory: true });
  if (dir) $("#outdir-input").value = dir;
}

/* ─── Project Save / Load (.kag) ─── */
function getExportConfig() {
  return {
    format: getFormatValue(),
    stitchMode: document.querySelector("#stitch-select .inline-option.selected")?.dataset?.value || "uniform",
    uniformWidth: parseInt($("#width-input").value) || 1200,
    borderWidth: parseInt($("#border-width-input").value) || 0,
    borderColor: $("#border-color-input").value || "#000000",
  };
}
async function saveProject() {
  if (files.length === 0) { toast("没有图片可保存", "error"); return; }
  const title = ($("#title-input").value || "").trim() || "Comic";
  const outdir = $("#outdir-input").value;
  const exportConfig = getExportConfig();

  const unlisten = await listen("save-progress", (event) => {
    const { current, total, file, stage } = event.payload;
    if (stage === "reading") {
      const pct = (current / total * 100).toFixed(1);
      elProgressFill.style.width = pct + "%";
      elProgressText.textContent = `读取中 ${current}/${total}: ${file}`;
    } else if (stage === "compressing") {
      elProgressFill.style.width = "66%";
      elProgressText.textContent = "正在压缩…";
    } else if (stage === "writing") {
      elProgressFill.style.width = "90%";
      elProgressText.textContent = "正在写入…";
    }
  });
  elBtnExport.disabled = true;
  elProgressArea.style.display = "flex";
  elProgressFill.style.width = "0%";
  elProgressText.textContent = "正在压缩…";

  try {
    const path = await invoke("save_project", { title, imagePaths: files, outdir, exportConfig });
    elProgressFill.style.width = "100%";
    elProgressText.textContent = "完成";
    toast("项目已保存: " + path.split(/[/\\]/).pop());
  } catch (e) {
    toast("保存失败: " + e, "error");
    elProgressText.textContent = "失败";
  } finally {
    unlisten();
    setTimeout(() => {
      elBtnExport.disabled = false;
      elProgressArea.style.display = "none";
    }, 1800);
  }
}
async function openProject() {
  const selected = await open({
    multiple: false,
    filters: [{ name: "Kitanga 项目", extensions: ["kag"] }],
  });
  if (!selected) return;
  elBtnExport.disabled = true;
  try {
    const result = await invoke("load_project", { kagPath: selected });
    files = result.images;
    selectedIndex = files.length > 0 ? 0 : -1;
    thumbDir = result.thumbDir;
    if (result.title) $("#title-input").value = result.title;
    if (result.exportSettings) {
      const exp = result.exportSettings;
      const fmtEl = $("#format-select");
      const fmtOpt = fmtEl.querySelector(`[data-value="${exp.format}"]`);
      if (fmtOpt) {
        fmtEl.querySelector(".custom-select-value").textContent = fmtOpt.textContent;
        fmtEl.dataset.value = exp.format;
      }
      const stitchOpt = document.querySelector(`#stitch-select .inline-option[data-value="${exp.stitchMode}"]`);
      if (stitchOpt) {
        document.querySelectorAll("#stitch-select .inline-option").forEach(o => o.classList.remove("selected"));
        stitchOpt.classList.add("selected");
      }
      if (exp.uniformWidth) $("#width-input").value = exp.uniformWidth;
      if (exp.borderWidth != null) $("#border-width-input").value = exp.borderWidth;
      if (exp.borderColor) $("#border-color-input").value = exp.borderColor;
    }
    toggleLongSettings();
    rebuildList();
    toast("已打开: " + selected.split(/[/\\]/).pop());
  } catch (e) {
    toast("打开失败: " + e, "error");
    thumbDir = null;
  } finally {
    elBtnExport.disabled = false;
  }
}

/* ─── Export ─── */
async function startExport() {
  if (files.length === 0) { toast("请先添加图片", "error"); return; }
  const format = getFormatValue();
  const title = ($("#title-input").value || "").trim() || "Comic";
  const outdir = $("#outdir-input").value;
  const stitchMode = document.querySelector("#stitch-select .inline-option.selected")?.dataset?.value || "uniform";
  const uniformWidth = parseInt($("#width-input").value) || 1200;
  const borderWidth = parseInt($("#border-width-input").value) || 0;
  const borderColor = $("#border-color-input").value || "#000000";

  elBtnExport.disabled = true;
  elProgressArea.style.display = "flex";
  elProgressFill.style.width = "0%";
  elProgressText.textContent = "正在处理…";

  try {
    const result = await invoke("export_images", { images: files, format, title, outdir, stitchMode, uniformWidth, borderWidth, borderColor });
    elProgressFill.style.width = "100%";
    elProgressText.textContent = "完成";
    toast("导出成功: " + result);
  } catch (e) {
    console.error("export error:", e);
    toast("导出失败: " + e, "error");
    elProgressText.textContent = "失败";
  } finally {
    setTimeout(() => {
      elBtnExport.disabled = false;
      elProgressArea.style.display = "none";
    }, 1800);
  }
}

/* ─── Custom Select ─── */
function getFormatValue() {
  return $("#format-select").dataset.value || "PDF";
}
function isLongImgFormat() {
  const v = getFormatValue();
  return v === "LongPNG" || v === "LongJPG" || v === "LongWEBP";
}
function toggleLongSettings() {
  const show = isLongImgFormat();
  document.querySelectorAll(".long-img-group").forEach(el => el.style.display = show ? "flex" : "none");
}

function initCustomSelect(onChange) {
  const el = $("#format-select");
  const trigger = el.querySelector(".custom-select-trigger");
  const valueSpan = el.querySelector(".custom-select-value");
  const options = el.querySelectorAll(".custom-select-option");

  function open() { el.classList.add("open"); }
  function close() { el.classList.remove("open"); }
  function select(opt) {
    options.forEach(o => o.classList.remove("selected"));
    opt.classList.add("selected");
    valueSpan.textContent = opt.textContent;
    el.dataset.value = opt.dataset.value;
    close();
    if (onChange) onChange(opt.dataset.value);
  }

  trigger.addEventListener("click", (e) => {
    e.stopPropagation();
    el.classList.contains("open") ? close() : open();
  });
  options.forEach(opt => {
    opt.addEventListener("click", (e) => {
      e.stopPropagation();
      select(opt);
    });
  });
  document.addEventListener("click", (e) => {
    if (!el.contains(e.target)) close();
  });
  el.addEventListener("keydown", (e) => {
    if (e.key === "Enter" || e.key === " ") { e.preventDefault(); open(); }
    if (e.key === "Escape") close();
  });
  const current = el.dataset.value;
  if (current) {
    const match = el.querySelector(`[data-value="${current}"]`);
    if (match) select(match);
  }
}

function initStitchSelect() {
  const opts = document.querySelectorAll("#stitch-select .inline-option");
  opts.forEach(opt => {
    opt.addEventListener("click", () => {
      opts.forEach(o => o.classList.remove("selected"));
      opt.classList.add("selected");
    });
  });
}

/* ─── Init ─── */
document.addEventListener("DOMContentLoaded", async () => {
  $("#btn-add").addEventListener("click", addFiles);
  $("#btn-add-folder").addEventListener("click", addFolder);
  $("#btn-outdir").addEventListener("click", chooseOutDir);
  $("#btn-move-up").addEventListener("click", moveUp);
  $("#btn-move-down").addEventListener("click", moveDown);
  $("#btn-remove").addEventListener("click", removeSelected);
  $("#btn-clear").addEventListener("click", clearAll);
  $("#btn-sort").addEventListener("click", sortFiles);
  $("#btn-save-project").addEventListener("click", saveProject);
  $("#btn-open-project").addEventListener("click", openProject);
  $("#btn-export").addEventListener("click", startExport);

  // Window controls
  const win = getCurrentWindow();
  $("#win-minimize").addEventListener("click", () => win.minimize());
  $("#win-maximize").addEventListener("click", () => win.toggleMaximize());
  $("#win-close").addEventListener("click", () => win.close());

  $("#title-input").addEventListener("keydown", (e) => {
    if (e.key === "Enter") startExport();
  });

  initCustomSelect(toggleLongSettings);
  initStitchSelect();
  toggleLongSettings();
  document.addEventListener("keydown", (e) => {
    if (e.target.tagName === "INPUT") return;
    switch (e.key) {
      case "Delete": case "Backspace": removeSelected(); break;
      case "ArrowUp": if (selectedIndex > 0) selectItem(selectedIndex - 1); break;
      case "ArrowDown": if (selectedIndex < files.length - 1) selectItem(selectedIndex + 1); break;
      case "o": if (e.ctrlKey) { e.preventDefault(); addFiles(); } break;
    }
  });

  document.body.addEventListener("dragover", (e) => e.preventDefault());
  document.body.addEventListener("drop", (e) => { e.preventDefault(); addFiles(); });

  try {
    const { desktopDir } = await import("@tauri-apps/api/path");
    $("#outdir-input").value = await desktopDir();
  } catch (_) {}
});
