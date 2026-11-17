// Mic capture: raw device I/O only. getUserMedia (so the browser's echo cancellation stays in
// the loop) plus the AudioWorklet tap. The PCM goes to Rust, which runs the VAD, decides when an
// utterance starts and ends, detects barge-in and does the speech-to-text. No audio logic here.

const RATE = 16000;
const QUANTA_PER_MESSAGE = 4; // worklet quanta (128 samples each) per message to Rust

export class Mic {
  private ctx?: AudioContext;
  private stream?: MediaStream;
  /** Bumped by every start() and stop(). A start that finds it changed is stale: it releases
   *  its own stream and does nothing else (CR-M1). Mute always ends with zero live tracks. */
  private gen = 0;
  private deviceId?: string;
  private muted = false;
  private pending: Float32Array[] = [];

  /** `onPcm` gets raw little-endian f32 samples (16 kHz mono). */
  constructor(private onPcm: (pcm: ArrayBuffer) => void) {}

  async start(deviceId?: string): Promise<void> {
    this.release();
    const gen = ++this.gen;
    this.deviceId = deviceId;
    if (this.muted) return; // stays closed until unmuted
    // Everything lives in locals until the very end, so a stale start can undo only itself.
    let stream: MediaStream | undefined;
    let ctx: AudioContext | undefined;
    const stale = () => gen !== this.gen || this.muted;
    const cleanup = () => {
      stream?.getTracks().forEach((t) => t.stop());
      void ctx?.close();
    };
    try {
      stream = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, deviceId: deviceId ? { exact: deviceId } : undefined },
      });
      if (stale()) return cleanup();
      ctx = new AudioContext({ sampleRate: RATE });
      await ctx.audioWorklet.addModule('/pcm-worklet.js');
      if (stale()) return cleanup();
      const src = ctx.createMediaStreamSource(stream);
      const tap = new AudioWorkletNode(ctx, 'pcm-tap');
      tap.port.onmessage = (m) => gen === this.gen && this.push(m.data as Float32Array);
      src.connect(tap); // not connected to destination: no self-monitoring
      this.stream = stream;
      this.ctx = ctx;
    } catch (e) {
      cleanup();
      if (gen === this.gen) throw e; // a stale start fails silently
    }
  }

  /** Stop capture and invalidate any start() still waiting on the device. */
  stop(): void {
    this.gen++;
    this.release();
  }

  /** Mute stops every track and closes the audio context, so the OS mic indicator goes off.
   *  Unmute acquires the device again (same device as before). */
  async setMuted(m: boolean): Promise<void> {
    if (m === this.muted) return;
    this.muted = m;
    if (m) {
      this.stop();
      return;
    }
    await this.start(this.deviceId);
  }

  private release(): void {
    this.stream?.getTracks().forEach((t) => t.stop());
    void this.ctx?.close();
    this.stream = undefined;
    this.ctx = undefined;
    this.pending = [];
  }

  private push(f: Float32Array): void {
    this.pending.push(f);
    if (this.pending.length < QUANTA_PER_MESSAGE) return;
    const out = new Float32Array(this.pending.reduce((n, q) => n + q.length, 0));
    let o = 0;
    for (const q of this.pending) {
      out.set(q, o);
      o += q.length;
    }
    this.pending = [];
    this.onPcm(out.buffer); // platform byte order is little-endian on every supported target
  }

  /** Live audio tracks held right now (for tests and diagnostics). */
  liveTracks(): number {
    return this.stream?.getTracks().filter((t) => t.readyState === 'live').length ?? 0;
  }
}
