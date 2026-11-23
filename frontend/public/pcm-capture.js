// AudioWorklet: turns microphone audio into 16 kHz, 16-bit mono PCM chunks (100 ms each) for the live voice session.
class PcmCapture extends AudioWorkletProcessor {
  constructor() {
    super();
    this.ratio = sampleRate / 16000; // input samples per output sample
    this.pos = 0;
    this.acc = 0;
    this.cnt = 0;
    this.out = new Int16Array(1600);
    this.n = 0;
  }

  process(inputs) {
    const x = inputs[0] && inputs[0][0];
    if (!x) return true;
    for (let i = 0; i < x.length; i++) {
      // Averaging the input samples that fall into one output sample is a cheap anti-aliasing low-pass filter.
      this.acc += x[i];
      this.cnt++;
      this.pos++;
      if (this.pos >= this.ratio) {
        this.pos -= this.ratio;
        const v = Math.max(-1, Math.min(1, this.acc / this.cnt));
        this.acc = 0;
        this.cnt = 0;
        this.out[this.n++] = v < 0 ? v * 0x8000 : v * 0x7fff;
        if (this.n === this.out.length) {
          this.port.postMessage(this.out.buffer.slice(0), []);
          this.n = 0;
        }
      }
    }
    return true;
  }
}

registerProcessor("pcm-capture", PcmCapture);
