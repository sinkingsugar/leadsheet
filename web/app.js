// leadsheet playground: drop MIDI, get text; edit text, hear MIDI.
// All conversion runs in leadsheet-wasm (same core as the CLI); audio is
// SpessaSynth + GeneralUser GS.

import init, { compress_midi, compress_jsonl, check, render, fmt } from "./pkg/leadsheet_wasm.js";
import {
  EditorView,
  EditorState,
  basicSetup,
  keymap,
  linter,
  lintGutter,
} from "./vendor/codemirror.js";
import { lsLanguage, lsHighlight, lsTheme } from "./ls-mode.js";
import { Player } from "./player.js";

const DEFAULT_LS = `# song: demo  tempo: 96.00  meter: 4/4  key: Am  grid: 1/16
# instruments: bass:33 drums:kit piano:0 lead:81

P1 bass   | A,,4 A,,4 G,,4 E,,4 |
P2 drums
  K |x... .... x.x. ....|
  S |.... x... .... x...|
  h |x.x. x.x. x.x. x.x.|
P3 piano* | Am . F G7 |
P4 lead   | e2 c2 d2 B2 c4 A4 |

arrangement:
  intro: [P1+P2] x2
  A:     [P1+P2+P3+P4] x4
`;

const $ = (id) => document.getElementById(id);
const playBtn = $("play-btn");
const pauseBtn = $("pause-btn");
const seekBar = $("seek");
const timeEl = $("time");
const sfChip = $("sf-chip");
const checkEl = $("check-status");
const extraEl = $("extra-status");

const player = new Player();
let songName = "demo";
let lastCheck = null; // parsed JSON of the last check() run
let dirtySincePlay = false;
let hasPlayed = false;

await init();

// ---------- editor ----------

function runCheck(text) {
  lastCheck = JSON.parse(check(text));
  renderStatus();
  return lastCheck;
}

function renderStatus() {
  if (!lastCheck) return;
  if (lastCheck.ok) {
    checkEl.className = "ok";
    checkEl.textContent =
      `✓ ${lastCheck.bars} bars · ${lastCheck.tracks} tracks · ` +
      `${lastCheck.notes} notes · ${lastCheck.bpm.toFixed(2)} BPM ${lastCheck.meter[0]}/${lastCheck.meter[1]}`;
    checkEl.onclick = null;
  } else {
    checkEl.className = "err";
    const d = lastCheck.diagnostics?.[0];
    if (d) {
      checkEl.textContent =
        `✗ line ${d.line}${d.col ? ":" + d.col : ""} ${d.message} [${d.code}]` +
        (d.suggestion ? ` — ${d.suggestion}` : "");
      checkEl.onclick = () => jumpTo(d.line, d.col);
    } else {
      checkEl.textContent = `✗ ${lastCheck.error}`;
      checkEl.onclick = null;
    }
  }
}

function jumpTo(line, col) {
  const doc = view.state.doc;
  if (line < 1 || line > doc.lines) return;
  const l = doc.line(line);
  const pos = Math.min(l.from + Math.max(0, (col || 1) - 1), l.to);
  view.dispatch({ selection: { anchor: pos }, scrollIntoView: true });
  view.focus();
}

const lsLinter = linter(
  (v) => {
    const text = v.state.doc.toString();
    const res = runCheck(text);
    dirtySincePlay = hasPlayed;
    updateTransport();
    if (res.ok || !res.diagnostics) return [];
    return res.diagnostics.map((d) => {
      const doc = v.state.doc;
      const line = d.line >= 1 && d.line <= doc.lines ? doc.line(d.line) : doc.line(1);
      const from = Math.min(line.from + Math.max(0, (d.col || 1) - 1), line.to);
      return {
        from,
        to: line.to > from ? line.to : Math.min(from + 1, doc.length),
        severity: "error",
        message: d.message + (d.suggestion ? `\nhelp: ${d.suggestion}` : ""),
        source: d.code,
      };
    });
  },
  { delay: 250 }
);

const view = new EditorView({
  state: EditorState.create({
    doc: DEFAULT_LS,
    extensions: [
      basicSetup,
      lsLanguage,
      lsHighlight,
      lsTheme,
      lintGutter(),
      lsLinter,
      keymap.of([
        {
          key: "Mod-Enter",
          run: () => {
            playCurrent();
            return true;
          },
        },
      ]),
    ],
  }),
  parent: $("editor"),
});
runCheck(DEFAULT_LS);

function setEditorText(text) {
  view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } });
  view.scrollDOM.scrollTop = 0;
  runCheck(text);
}

function editorText() {
  return view.state.doc.toString();
}

function songNameFromText(text) {
  const m = text.match(/^#\s*song:\s*(\S+)/m);
  return m ? m[1] : songName;
}

// ---------- file handling ----------

function compressStatus(res, fileName) {
  const kb = (res.lsBytes / 1024).toFixed(1);
  const ratio = res.naiveBytes / Math.max(1, res.lsBytes);
  let msg = `${fileName}: ${res.notes} notes → ${kb} KB text (${ratio.toFixed(1)}× vs naive)`;
  if (res.tempoSource.kind === "auto-inferred") {
    msg += ` · declared ${res.tempoSource.declaredBpm.toFixed(2)} BPM fit poorly, using inferred ${res.bpm.toFixed(2)}`;
  }
  extraEl.textContent = msg;
}

async function openFile(file) {
  const name = file.name;
  const stem = name.replace(/\.[^.]+$/, "");
  const ext = (name.match(/\.([^.]+)$/)?.[1] ?? "").toLowerCase();
  try {
    if (ext === "mid" || ext === "midi") {
      const bytes = new Uint8Array(await file.arrayBuffer());
      const res = JSON.parse(compress_midi(bytes, stem, undefined, false, false));
      if (!res.ok) throw new Error(res.error);
      songName = stem;
      setEditorText(res.text);
      compressStatus(res, name);
    } else if (ext === "jsonl") {
      const res = JSON.parse(compress_jsonl(await file.text(), stem, undefined, false, false));
      if (!res.ok) throw new Error(res.error);
      songName = stem;
      setEditorText(res.text);
      compressStatus(res, name);
    } else if (ext === "ls" || ext === "txt") {
      songName = stem;
      setEditorText(await file.text());
      extraEl.textContent = `opened ${name}`;
    } else if (ext === "sf2" || ext === "sf3" || ext === "dls") {
      sfChip.className = "chip loading";
      sfChip.textContent = `loading ${name}…`;
      await player.loadSoundFont(await file.arrayBuffer(), name);
      sfChip.className = "chip loaded";
      sfChip.textContent = `soundfont: ${name}`;
      extraEl.textContent = `soundfont swapped to ${name}`;
    } else {
      extraEl.textContent = `unsupported file: ${name}`;
    }
  } catch (e) {
    extraEl.textContent = `${name}: ${e.message ?? e}`;
  }
}

$("open-btn").onclick = () => $("file-input").click();
$("file-input").onchange = (e) => {
  if (e.target.files[0]) openFile(e.target.files[0]);
  e.target.value = "";
};

const dropzone = $("dropzone");
let dragDepth = 0;
window.addEventListener("dragenter", (e) => {
  e.preventDefault();
  dragDepth++;
  dropzone.hidden = false;
});
window.addEventListener("dragleave", (e) => {
  e.preventDefault();
  if (--dragDepth <= 0) {
    dragDepth = 0;
    dropzone.hidden = true;
  }
});
window.addEventListener("dragover", (e) => e.preventDefault());
window.addEventListener("drop", (e) => {
  e.preventDefault();
  dragDepth = 0;
  dropzone.hidden = true;
  const file = e.dataTransfer?.files?.[0];
  if (file) openFile(file);
});

$("example-sel").onchange = async (e) => {
  const name = e.target.value;
  e.target.selectedIndex = 0;
  try {
    const resp = await fetch(`examples/${name}.ls`);
    if (!resp.ok) throw new Error(`HTTP ${resp.status} (local dev: run web/build.sh first)`);
    songName = name;
    setEditorText(await resp.text());
    extraEl.textContent = `example: ${name}`;
  } catch (err) {
    extraEl.textContent = `example ${name}: ${err.message}`;
  }
};

// ---------- downloads / fmt ----------

function download(bytes, filename, type) {
  const url = URL.createObjectURL(new Blob([bytes], { type }));
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}

$("dl-ls-btn").onclick = () => {
  const text = editorText();
  download(text, `${songNameFromText(text)}.ls`, "text/plain");
};

$("dl-mid-btn").onclick = () => {
  const text = editorText();
  try {
    download(render(text), `${songNameFromText(text)}.mid`, "audio/midi");
  } catch (e) {
    extraEl.textContent = `render: fix the errors first`;
  }
};

$("fmt-btn").onclick = () => {
  try {
    setEditorText(fmt(editorText()));
    extraEl.textContent = "formatted (canonical form)";
  } catch (e) {
    extraEl.textContent = "fmt: fix the errors first";
  }
};

// ---------- playback ----------

function fmtTime(s) {
  s = Math.max(0, Math.round(s));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

function updateTransport() {
  playBtn.textContent = hasPlayed && dirtySincePlay ? "▶ update" : "▶ play";
}

async function ensureSoundFont() {
  if (player.hasSoundFont) return;
  sfChip.className = "chip loading";
  await player.loadDefaultSoundFont((got, total) => {
    const pct = total ? Math.round((100 * got) / total) : 0;
    sfChip.textContent = total
      ? `GeneralUser GS ${pct}% of ${(total / 1048576).toFixed(0)} MB`
      : `GeneralUser GS ${(got / 1048576).toFixed(1)} MB…`;
  });
  sfChip.className = "chip loaded";
  sfChip.textContent = "soundfont: GeneralUser GS";
}

async function playCurrent() {
  player.unlock(); // synchronously, while the user gesture is still live (Safari)
  const text = editorText();
  let bytes;
  try {
    bytes = render(text);
  } catch (e) {
    extraEl.textContent = "can't play: fix the errors first";
    return;
  }
  playBtn.disabled = true;
  try {
    await ensureSoundFont();
    await player.play(bytes, `${songNameFromText(text)}.mid`);
    hasPlayed = true;
    dirtySincePlay = false;
    pauseBtn.disabled = false;
    seekBar.disabled = false;
    pauseBtn.textContent = "⏸";
    updateTransport();
  } catch (e) {
    extraEl.textContent = `audio: ${e.message ?? e}`;
    sfChip.className = "chip";
    sfChip.textContent = "soundfont: failed — drop a .sf2";
  } finally {
    playBtn.disabled = false;
  }
}

playBtn.onclick = playCurrent;

pauseBtn.onclick = () => {
  if (player.paused) {
    player.resume();
    pauseBtn.textContent = "⏸";
  } else {
    player.pause();
    pauseBtn.textContent = "▶";
  }
};

let seeking = false;
seekBar.oninput = () => {
  seeking = true;
  timeEl.textContent = `${fmtTime((seekBar.value / 1000) * player.duration)} / ${fmtTime(player.duration)}`;
};
seekBar.onchange = () => {
  player.seek((seekBar.value / 1000) * player.duration);
  seeking = false;
};

// console debugging handle
window.__ls = { view: () => view, player };

(function tick() {
  if (player.active && !seeking) {
    const dur = player.duration;
    seekBar.value = dur ? Math.round((1000 * player.currentTime) / dur) : 0;
    timeEl.textContent = `${fmtTime(player.currentTime)} / ${fmtTime(dur)}`;
    if (player.finished) pauseBtn.textContent = "▶";
  }
  requestAnimationFrame(tick);
})();
