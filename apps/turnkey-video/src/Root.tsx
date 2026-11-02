import React from "react";
import { Composition } from "remotion";
import { DURATION, Turnkey } from "./Turnkey";

export const Root: React.FC = () => (
  <Composition id="Turnkey" component={Turnkey} durationInFrames={DURATION} fps={30} width={1280} height={720} />
);
