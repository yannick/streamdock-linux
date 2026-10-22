// The built-in player's stream format as text (H15-73), separate from Player.tsx so it can be tested.
import type { PlayerState } from "../gen/streamdock/v1/device_pb.js";

function khz(rate: number): string {
  return `${(rate / 1000).toFixed(rate % 1000 ? 1 : 0)} kHz`;
}

/**
 * "FLAC 48 kHz · converted by Lyrion from 44.1 kHz · 1411kbps": the stream as decoded, where its
 * rate was changed (by Lyrion on the server, or by PipeWire on the board, H15-73) and the bitrate.
 */
export function formatText(state: Pick<PlayerState, "format" | "bitrate" | "sampleRate" | "sourceSampleRate" | "outputSampleRate">): string {
  const { sampleRate: rate, sourceSampleRate: source, outputSampleRate: output } = state;
  const stream = rate && !state.format.startsWith("PCM") ? `${state.format} ${khz(rate)}` : state.format;
  const parts = [
    stream,
    rate && source && source !== rate ? `converted by Lyrion from ${khz(source)}` : "",
    rate && output && output !== rate ? `resampled here to ${khz(output)}` : "",
    state.bitrate,
  ];
  return parts.filter(Boolean).join(" · ");
}

/** The Lyrion rule that resamples FLAC to `rate` on the server for this player only (H15-73). */
export function serverRule(playerId: string, rate = 48000): string {
  return [
    `# StreamDock (H15-73): FLAC resampled to ${rate / 1000} kHz on the server`,
    `flc flc * ${playerId}`,
    "\t# FT:{START=--skip=%t}U:{END=--until=%v}",
    `\t[flac] -dcs $START$ $END$ -- $FILE$ | [sox] -q -t wav - -t flac -C 0 - rate -v ${rate}`,
  ].join("\n");
}
