// SpessaSynth wrapper: one synth, one sequencer, one soundfont slot.
// GeneralUser GS is fetched lazily on first play and kept in the Cache API,
// so the 32 MB download happens once per browser.

import { WorkletSynthesizer, Sequencer } from "./vendor/spessasynth.js";

const SF_URL =
  "https://raw.githubusercontent.com/mrbumpy409/GeneralUser-GS/684543d5e5efaef08d02be50dcda8d552478fa60/GeneralUser-GS.sf2";
const SF_CACHE = "leadsheet-soundfont-v1";
const SF_BANK_ID = "main";

async function fetchWithCache(url, onProgress) {
  let cache = null;
  try {
    cache = await caches.open(SF_CACHE);
    const hit = await cache.match(url);
    if (hit) return await hit.arrayBuffer();
  } catch {
    // Cache API unavailable (e.g. plain-http host) — plain fetch still works.
  }
  const resp = await fetch(url);
  if (!resp.ok) throw new Error(`soundfont fetch failed: HTTP ${resp.status}`);
  const total = Number(resp.headers.get("content-length")) || 0;
  const reader = resp.body.getReader();
  const chunks = [];
  let got = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    got += value.length;
    onProgress?.(got, total);
  }
  const blob = new Blob(chunks);
  if (cache) await cache.put(url, new Response(blob)).catch(() => {});
  return await blob.arrayBuffer();
}

export class Player {
  ctx = null;
  synth = null;
  seq = null;
  soundFontName = null;

  async init() {
    if (this.ctx) return;
    this.ctx = new AudioContext();
    await this.ctx.audioWorklet.addModule("./vendor/spessasynth_processor.min.js");
    this.synth = new WorkletSynthesizer(this.ctx);
    this.synth.connect(this.ctx.destination);
  }

  get hasSoundFont() {
    return this.soundFontName !== null;
  }

  async loadDefaultSoundFont(onProgress) {
    const buf = await fetchWithCache(SF_URL, onProgress);
    await this.loadSoundFont(buf, "GeneralUser GS");
  }

  async loadSoundFont(buffer, name) {
    await this.init();
    if (this.hasSoundFont) {
      try {
        this.synth.soundBankManager.deleteSoundBank(SF_BANK_ID);
      } catch {
        // fine — replacing is best-effort, addSoundBank below is what matters
      }
    }
    await this.synth.soundBankManager.addSoundBank(buffer, SF_BANK_ID);
    await this.synth.isReady;
    this.soundFontName = name;
  }

  // Load rendered MIDI bytes and start playing from the top.
  async play(midiBytes, name) {
    await this.init();
    await this.ctx.resume();
    if (!this.seq) {
      this.seq = new Sequencer(this.synth, { skipToFirstNoteOn: false });
      this.seq.loopCount = 0;
    }
    const binary = midiBytes.buffer.slice(
      midiBytes.byteOffset,
      midiBytes.byteOffset + midiBytes.byteLength
    );
    this.seq.loadNewSongList([{ binary, fileName: name }]);
    this.seq.loopCount = 0;
    this.seq.play();
    // Re-anchor the sequencer clock: on a cold start the soundfont download
    // happens between AudioContext creation and play(), and currentTime
    // would otherwise read as context-age, i.e. past the end of the song.
    this.seq.currentTime = 0;
  }

  get active() {
    return this.seq !== null && this.seq.midiData !== undefined;
  }
  get paused() {
    return this.seq?.paused ?? true;
  }
  get finished() {
    return this.seq?.isFinished ?? false;
  }
  get duration() {
    return this.seq?.duration ?? 0;
  }
  get currentTime() {
    return this.seq ? Math.min(this.seq.currentTime, this.duration) : 0;
  }

  pause() {
    this.seq?.pause();
  }
  resume() {
    this.ctx?.resume();
    this.seq?.play();
  }
  seek(seconds) {
    if (this.seq) this.seq.currentTime = seconds;
  }
  stopNotes() {
    this.synth?.stopAll?.();
  }
}
