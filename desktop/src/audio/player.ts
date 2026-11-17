// Plays streamed PCM16 mono (22.05 kHz) through an AnalyserNode so the orb can
// react to the real output level/FFT. Stop is immediate.
//
// The AudioContext is created on the first user gesture (pointerdown / keydown), not at module
// load: WebView2 keeps a context made outside a gesture suspended, and the user hears nothing.
// It is also resumed before every playback.

const RATE = 22050;

export class Player {
  private ctx: AudioContext | null = null;
  private analyser: AnalyserNode | null = null;
  private nextTime = 0;
  private sources = new Set<AudioBufferSourceNode>();
  private carry: Uint8Array | null = null;
  private fft = new Uint8Array(0);
  private gen = 0; // bumped by stop(): chunks still waiting for the context are dropped

  constructor() {
    const unlock = () => void this.wake().catch(() => {});
    window.addEventListener('pointerdown', unlock, { capture: true });
    window.addEventListener('keydown', unlock, { capture: true });
  }

  /** Create the context if needed and resume it. Call from a user gesture, and before playing. */
  async wake(): Promise<AudioContext> {
    if (!this.ctx) {
      const ctx = new AudioContext();
      const analyser = ctx.createAnalyser();
      analyser.fftSize = 512;
      analyser.smoothingTimeConstant = 0.6;
      analyser.connect(ctx.destination);
      this.fft = new Uint8Array(analyser.frequencyBinCount);
      this.ctx = ctx;
      this.analyser = analyser;
    }
    if (this.ctx.state !== 'running') await this.ctx.resume();
    return this.ctx;
  }

  /** Push one raw PCM16LE chunk (may split a sample across chunks). */
  push(chunk: ArrayBuffer): void {
    let bytes = new Uint8Array(chunk);
    if (this.carry) {
      const merged = new Uint8Array(this.carry.length + bytes.length);
      merged.set(this.carry);
      merged.set(bytes, this.carry.length);
      bytes = merged;
      this.carry = null;
    }
    if (bytes.length % 2) {
      this.carry = bytes.slice(bytes.length - 1);
      bytes = bytes.slice(0, bytes.length - 1);
    }
    if (!bytes.length) return;
    const pcm = new Int16Array(bytes.buffer, bytes.byteOffset, bytes.length / 2);
    const gen = this.gen;
    void this.wake()
      .then((ctx) => {
        if (gen !== this.gen) return;
        const buf = ctx.createBuffer(1, pcm.length, RATE);
        const ch = buf.getChannelData(0);
        for (let i = 0; i < pcm.length; i++) ch[i] = pcm[i] / 32768;
        this.schedule(ctx, buf);
      })
      .catch(() => {});
  }

  private schedule(ctx: AudioContext, buf: AudioBuffer): void {
    const src = ctx.createBufferSource();
    src.buffer = buf;
    src.connect(this.analyser!);
    const at = Math.max(ctx.currentTime + 0.02, this.nextTime);
    src.start(at);
    this.nextTime = at + buf.duration;
    this.sources.add(src);
    src.onended = () => this.sources.delete(src);
  }

  /** True while audio is queued or playing. */
  get active(): boolean {
    if (!this.ctx) return false;
    return this.sources.size > 0 || this.nextTime > this.ctx.currentTime + 0.01;
  }

  /** Level 0..1 and FFT of what is coming out of the speakers right now. */
  read(): { level: number; fft: Uint8Array } {
    if (!this.analyser) return { level: 0, fft: this.fft };
    this.analyser.getByteFrequencyData(this.fft);
    let sum = 0;
    for (const v of this.fft) sum += v;
    return { level: Math.min(1, (sum / this.fft.length / 255) * 2.5), fft: this.fft };
  }

  stop(): void {
    for (const s of this.sources) {
      s.onended = null;
      try {
        s.stop();
      } catch {
        /* already stopped */
      }
    }
    this.gen++;
    this.sources.clear();
    this.carry = null;
    this.nextTime = 0;
  }
}
