/**
 * DeepSeek 语音能力工具：
 * - 录音并编码为 16kHz / 16-bit / 单声道 PCM WAV（符合 DeepSeek ASR 要求）
 * - 顺序播放分段 Base64 WAV 音频（支持 DeepSeek TTS 长回复分段朗读）
 */

const TARGET_SAMPLE_RATE = 16000;
const MAX_RECORD_SECONDS = 120;

export interface VoiceRecorder {
  stop: () => Promise<{ base64: string; durationMs: number }>;
  cancel: () => void;
}

function downsampleBuffer(samples: Float32Array, inputRate: number, outputRate: number): Float32Array {
  if (inputRate === outputRate) return samples;
  const ratio = inputRate / outputRate;
  const newLen = Math.max(1, Math.round(samples.length / ratio));
  const result = new Float32Array(newLen);
  let offsetResult = 0;
  let offsetBuffer = 0;
  while (offsetResult < result.length) {
    const nextOffsetBuffer = Math.min(samples.length, Math.round((offsetResult + 1) * ratio));
    let accum = 0;
    let count = 0;
    for (let i = offsetBuffer; i < nextOffsetBuffer; i++) {
      accum += samples[i];
      count++;
    }
    result[offsetResult] = count > 0 ? accum / count : 0;
    offsetResult++;
    offsetBuffer = nextOffsetBuffer;
  }
  return result;
}

function encodeWav16BitMono(samples: Float32Array, sampleRate: number): Uint8Array {
  const dataBytes = samples.length * 2;
  const buffer = new ArrayBuffer(44 + dataBytes);
  const view = new DataView(buffer);

  const writeStr = (offset: number, s: string) => {
    for (let i = 0; i < s.length; i++) view.setUint8(offset + i, s.charCodeAt(i));
  };

  writeStr(0, "RIFF");
  view.setUint32(4, 36 + dataBytes, true);
  writeStr(8, "WAVE");
  writeStr(12, "fmt ");
  view.setUint32(16, 16, true); // PCM chunk size
  view.setUint16(20, 1, true); // PCM format
  view.setUint16(22, 1, true); // mono
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * 2, true); // byte rate
  view.setUint16(32, 2, true); // block align
  view.setUint16(34, 16, true); // bits per sample
  writeStr(36, "data");
  view.setUint32(40, dataBytes, true);

  let offset = 44;
  for (let i = 0; i < samples.length; i++) {
    const clamped = Math.max(-1, Math.min(1, samples[i]));
    view.setInt16(offset, clamped < 0 ? clamped * 0x8000 : clamped * 0x7fff, true);
    offset += 2;
  }
  return new Uint8Array(buffer);
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

export async function startVoiceRecording(
  onTick?: (elapsedSec: number) => void,
  onMaxReached?: () => void,
): Promise<VoiceRecorder> {
  if (!navigator.mediaDevices?.getUserMedia) {
    throw new Error("当前环境不支持麦克风录音");
  }
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true },
  });
  const ctx = new AudioContext();
  const source = ctx.createMediaStreamSource(stream);
  const processor = ctx.createScriptProcessor(4096, 1, 1);
  const silentGain = ctx.createGain();
  silentGain.gain.value = 0;
  const chunks: Float32Array[] = [];
  const startedAt = Date.now();
  let stopped = false;

  processor.onaudioprocess = (ev) => {
    if (stopped) return;
    const input = ev.inputBuffer.getChannelData(0);
    chunks.push(new Float32Array(input));
  };

  source.connect(processor);
  processor.connect(silentGain);
  silentGain.connect(ctx.destination);

  const timer = window.setInterval(() => {
    const elapsed = Math.floor((Date.now() - startedAt) / 1000);
    onTick?.(elapsed);
    if (elapsed >= MAX_RECORD_SECONDS) {
      onMaxReached?.();
    }
  }, 300);

  const cleanup = () => {
    stopped = true;
    window.clearInterval(timer);
    try {
      processor.disconnect();
      source.disconnect();
      silentGain.disconnect();
    } catch {
      // ignore
    }
    stream.getTracks().forEach((t) => t.stop());
    void ctx.close().catch(() => {});
  };

  return {
    stop: async () => {
      const durationMs = Date.now() - startedAt;
      const sampleRate = ctx.sampleRate;
      cleanup();
      const total = chunks.reduce((sum, arr) => sum + arr.length, 0);
      const merged = new Float32Array(total);
      let pos = 0;
      for (const c of chunks) {
        merged.set(c, pos);
        pos += c.length;
      }
      const downsampled = downsampleBuffer(merged, sampleRate, TARGET_SAMPLE_RATE);
      const wav = encodeWav16BitMono(downsampled, TARGET_SAMPLE_RATE);
      return { base64: bytesToBase64(wav), durationMs };
    },
    cancel: () => {
      cleanup();
    },
  };
}

let activeStopPlayback: (() => void) | null = null;

export function stopActiveTtsPlayback() {
  if (activeStopPlayback) {
    activeStopPlayback();
    activeStopPlayback = null;
  }
}

export function playBase64WavChunks(
  chunks: string[],
  onEnded?: () => void,
  onError?: (err: string) => void,
): () => void {
  stopActiveTtsPlayback();

  let cancelled = false;
  let currentAudio: HTMLAudioElement | null = null;
  let currentUrl: string | null = null;

  const stop = () => {
    cancelled = true;
    if (currentAudio) {
      currentAudio.pause();
      currentAudio.src = "";
      currentAudio = null;
    }
    if (currentUrl) {
      URL.revokeObjectURL(currentUrl);
      currentUrl = null;
    }
    if (activeStopPlayback === stop) {
      activeStopPlayback = null;
    }
  };
  activeStopPlayback = stop;

  const playNext = (index: number) => {
    if (cancelled) return;
    if (index >= chunks.length) {
      stop();
      onEnded?.();
      return;
    }
    try {
      const bin = atob(chunks[index]);
      const bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      const blob = new Blob([bytes], { type: "audio/wav" });
      currentUrl = URL.createObjectURL(blob);
      const audio = new Audio(currentUrl);
      currentAudio = audio;
      audio.onended = () => {
        if (currentUrl) {
          URL.revokeObjectURL(currentUrl);
          currentUrl = null;
        }
        playNext(index + 1);
      };
      audio.onerror = () => {
        stop();
        onError?.("音频播放失败");
      };
      void audio.play().catch((e) => {
        stop();
        onError?.(`无法播放音频：${String(e)}`);
      });
    } catch (e) {
      stop();
      onError?.(`音频解码失败：${String(e)}`);
    }
  };

  playNext(0);
  return stop;
}
