import React from "react";
import { AbsoluteFill, Series } from "remotion";
import { Approve } from "./scenes/Approve";
import { Closing, Title } from "./scenes/Cards";
import { Claim } from "./scenes/Claim";
import { Connect } from "./scenes/Connect";
import { Machine } from "./scenes/Machine";
import { Server } from "./scenes/Server";
import { c } from "./theme";

/** The scenes in order, with their lengths in frames at 30 fps. */
const SCENES: [React.FC<{ dur: number }>, number][] = [
  [Title, 96],
  [Server, 330],
  [Claim, 300],
  [Connect, 285],
  [Machine, 285],
  [Approve, 315],
  [Closing, 104],
];

export const DURATION = SCENES.reduce((sum, [, dur]) => sum + dur, 0);

export const Turnkey: React.FC = () => (
  <AbsoluteFill style={{ background: c.bg }}>
    <Series>
      {SCENES.map(([Scene, dur], i) => (
        <Series.Sequence key={i} durationInFrames={dur}>
          <Scene dur={dur} />
        </Series.Sequence>
      ))}
    </Series>
  </AbsoluteFill>
);
