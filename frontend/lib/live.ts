import { API, type Lang, type TokenProvider } from "./api";
import { logger } from "./log";

const log = logger("live");

export type LiveState = "listening" | "speaking";
export type LiveEvents = {
  onState: (s: LiveState) => void;
  onUserText: (t: string) => void;
  onAssistantText: (t: string) => void;
  onTurnComplete: () => void;
  onTool: (name: string) => void;
  onEmergency: (text: string) => void;
  onError: (code: string, message: string) => void;
  onClosed: () => void;
};

/** Gemini sometimes repeats a caption twice in a row; collapse an exact doubling. */
export function tidyCaption(t: string): string {
  const s = t.trim();
  const h = s.length / 2;
  return s.length >= 8 && Number.isInteger(h) && s.slice(0, h) === s.slice(h) ? s.slice(0, h) : s;
}

/**
 * Speech-to-speech conversation. Microphone audio (16 kHz PCM) streams to the backend, which relays it to Gemini
 * Live; the model's voice (24 kHz PCM) streams back and plays immediately. Nothing is converted to text on the way.
 * Talking while the model speaks interrupts it, like a phone call.
 */
// Voice activity gate: silence is not streamed (the model is billed for audio it receives), and a session with nobody
// talking costs nothing. A little audio before speech starts and ~1.2 s of trailing silence are kept so the model
// still hears the start of a sentence and can tell when it ended.
const PRE_CHUNKS = 3;
const HANG_CHUNKS = 12;
const MIN_THRESHOLD = 0.012;

export class LiveSession {
  private active = false;
  private hang = 0;
  private pre: ArrayBuffer[] = [];
  private floor = 0.004;
  private ws: WebSocket | null = null;
  private mic: MediaStream | null = null;
  private capCtx: AudioContext | null = null;
  private outCtx: AudioContext | null = null;
  private worklet: AudioWorkletNode | null = null;
  private sources = new Set<AudioBufferSourceNode>();
  private nextTime = 0;
  private ready = false;
  private closed = false;
  private state: LiveState = "listening";
  private ev!: LiveEvents;

  async start(getToken: TokenProvider, lang: Lang, ev: LiveEvents): Promise<void> {
    this.ev = ev;
    // The microphone request must come straight from the click, so it goes first.
    this.mic = await navigator.mediaDevices.getUserMedia({
      audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true, channelCount: 1 },
    });
    this.outCtx = new AudioContext({ sampleRate: 24000 });
    this.capCtx = new AudioContext();
    await Promise.all([this.outCtx.resume(), this.capCtx.resume()]);
    await this.capCtx.audioWorklet.addModule("/pcm-capture.js");
    const source = this.capCtx.createMediaStreamSource(this.mic);
    this.worklet = new AudioWorkletNode(this.capCtx, "pcm-capture");
    const mute = this.capCtx.createGain(); // keeps the graph running without playing the mic back
    mute.gain.value = 0;
    source.connect(this.worklet).connect(mute).connect(this.capCtx.destination);
    this.worklet.port.onmessage = (e: MessageEvent<ArrayBuffer>) => this.onMic(e.data);

    const token = await getToken();
    if (!token) throw new Error("Please sign in again.");
    const url = `${API.replace(/^http/, "ws")}/api/live`;
    const ws = new WebSocket(url);
    ws.binaryType = "arraybuffer";
    this.ws = ws;
    ws.onopen = () => { log.info("connected, authenticating"); ws.send(JSON.stringify({ type: "auth", token, lang })); };
    ws.onmessage = (e) => (typeof e.data === "string" ? this.onControl(JSON.parse(e.data)) : this.onAudio(e.data as ArrayBuffer));
    ws.onerror = () => { log.error(`could not reach the voice service at ${url} (is the backend running?)`); this.fail("connection", "Could not connect to the voice service."); };
    ws.onclose = () => this.teardown();
  }

  private onMic(buf: ArrayBuffer) {
    const ws = this.ws;
    if (!this.ready || !ws || ws.readyState !== WebSocket.OPEN) return;
    const pcm = new Int16Array(buf);
    let sum = 0;
    for (let i = 0; i < pcm.length; i++) sum += pcm[i] * pcm[i];
    const rms = Math.sqrt(sum / pcm.length) / 0x8000;
    const speaking = rms > Math.max(MIN_THRESHOLD, this.floor * 3);
    if (!speaking) this.floor = this.floor * 0.95 + rms * 0.05; // learn the room's background noise
    if (speaking) {
      if (!this.active) { this.pre.forEach((c) => ws.send(c)); this.pre = []; this.active = true; }
      ws.send(buf);
      this.hang = HANG_CHUNKS;
    } else if (this.active) {
      ws.send(buf);
      if (--this.hang <= 0) { this.active = false; ws.send(JSON.stringify({ type: "audio_end" })); }
    } else {
      this.pre.push(buf);
      if (this.pre.length > PRE_CHUNKS) this.pre.shift();
    }
  }

  stop() {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify({ type: "end" }));
    this.teardown();
  }

  private fail(code: string, message: string) {
    if (this.closed) return;
    this.ev.onError(code, message);
    this.teardown();
  }

  private setState(s: LiveState) {
    if (this.state !== s) { this.state = s; this.ev.onState(s); }
  }

  private onControl(m: { type: string; text?: string; name?: string; code?: string; message?: string }) {
    switch (m.type) {
      case "ready": log.info("voice session ready"); this.ready = true; this.state = "listening"; this.ev.onState("listening"); break;
      case "user_text": this.ev.onUserText(m.text ?? ""); break;
      case "assistant_text": this.ev.onAssistantText(m.text ?? ""); break;
      case "turn_complete": this.ev.onTurnComplete(); break;
      case "tool": this.ev.onTool(m.name ?? ""); break;
      case "emergency": this.ev.onEmergency(m.text ?? ""); break;
      case "interrupted": this.stopPlayback(); this.setState("listening"); break;
      case "error": log.warn(`server error ${m.code}`); this.fail(m.code ?? "error", m.message ?? "Voice session failed."); break;
    }
  }

  private onAudio(buf: ArrayBuffer) {
    const ctx = this.outCtx;
    if (!ctx || this.closed || buf.byteLength < 2) return;
    const pcm = new Int16Array(buf, 0, Math.floor(buf.byteLength / 2));
    const data = new Float32Array(pcm.length);
    for (let i = 0; i < pcm.length; i++) data[i] = pcm[i] / 0x8000;
    const ab = ctx.createBuffer(1, data.length, 24000);
    ab.copyToChannel(data, 0);
    const src = ctx.createBufferSource();
    src.buffer = ab;
    src.connect(ctx.destination);
    this.nextTime = Math.max(ctx.currentTime + 0.02, this.nextTime);
    src.start(this.nextTime);
    this.nextTime += ab.duration;
    this.sources.add(src);
    src.onended = () => { this.sources.delete(src); if (this.sources.size === 0) this.setState("listening"); };
    this.setState("speaking");
  }

  private stopPlayback() {
    for (const s of this.sources) { s.onended = null; try { s.stop(); } catch { /* already stopped */ } }
    this.sources.clear();
    this.nextTime = 0;
  }

  private teardown() {
    if (this.closed) return;
    this.closed = true;
    this.ready = false;
    this.stopPlayback();
    this.worklet?.port.close();
    this.mic?.getTracks().forEach((t) => t.stop());
    void this.capCtx?.close().catch(() => { });
    void this.outCtx?.close().catch(() => { });
    if (this.ws && this.ws.readyState <= WebSocket.OPEN) this.ws.close();
    this.ev?.onClosed();
  }
}
