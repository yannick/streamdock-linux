import { describe, expect, it } from "vitest";

import { formatText } from "./playerFormat.js";

const base = { format: "FLAC", bitrate: "", sampleRate: 0, sourceSampleRate: 0, outputSampleRate: 0 };

describe("formatText", () => {
  it("names a conversion on the server", () => {
    expect(formatText({ ...base, sampleRate: 48000, sourceSampleRate: 44100, outputSampleRate: 48000 })).toBe(
      "FLAC 48 kHz · converted by Lyrion from 44.1 kHz",
    );
  });
  it("names resampling on the board", () => {
    expect(formatText({ ...base, sampleRate: 44100, sourceSampleRate: 44100, outputSampleRate: 48000, bitrate: "1411kbps" })).toBe(
      "FLAC 44.1 kHz · resampled here to 48 kHz · 1411kbps",
    );
  });
  it("keeps PCM's own rate and unknown rates out", () => {
    expect(formatText({ ...base, format: "PCM 44.1 kHz 16 bit", sampleRate: 44100 })).toBe("PCM 44.1 kHz 16 bit");
    expect(formatText(base)).toBe("FLAC");
  });
});
