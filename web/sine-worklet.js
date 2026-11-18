// 最小对照测试：纯原生 AudioWorklet 正弦，不加载 wasm/素材，不做任何分配。
// 在 Firefox Android 上打开 /?test=sine 后点播放；若这个也卡，说明是浏览器自身问题。
class SineProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.playing = false;
    this.phase = 0;
    this.last = currentTime;
    this.samples = 0;
    this.port.onmessage = (e) => {
      if (e.data === "play") this.playing = true;
      else if (e.data === "pause") this.playing = false;
    };
    this.port.postMessage({ type: "hello" });
  }

  process(inputs, outputs) {
    const out = outputs[0];
    if (!out || !out[0]) return true;
    const L = out[0], R = out[1] || out[0];
    if (!this.playing) {
      L.fill(0);
      if (R !== L) R.fill(0);
      return true;
    }
    const inc = 2 * Math.PI * 440 / sampleRate;
    for (let i = 0; i < L.length; i++) {
      const s = 0.12 * Math.sin(this.phase);
      L[i] = s;
      R[i] = s;
      this.phase += inc;
      if (this.phase > 2 * Math.PI) this.phase -= 2 * Math.PI;
    }
    this.samples += L.length;
    const now = currentTime;
    if (now - this.last > 1) {
      const dt = now - this.last;
      this.port.postMessage({
        type: "tick",
        actualSr: this.samples / dt,
        targetSr: sampleRate,
        quantum: L.length,
      });
      this.samples = 0;
      this.last = now;
    }
    return true;
  }
}

registerProcessor("sine-processor", SineProcessor);
